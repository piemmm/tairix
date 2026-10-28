//! `QUERY_INFO` and `SET_INFO` (MS-SMB2 §2.2.37–§2.2.40) and the
//! information classes this client reads and writes (MS-FSCC §2.4, §2.5).
//!
//! A fixed-size class is accepted only at its exact length; a variable one —
//! a filesystem's name, a volume's label — only within its bound.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::time::Time64;
use tairix_util::flags;

use crate::file::name::SharePath;
use crate::file::{
    exactly, output_buffer, read_filetime, write_filetime, write_output_buffer, FileAttributes,
    FileId, FileTimes,
};
use crate::header::{structure, HEADER_LEN};
use crate::wire::{
    buffer_at, filetime_exact, from_utf16le, owned, time_of_filetime, utf16le, utf16le_len,
    Encoder, Reader,
};
use crate::SmbError;

/// What a query or a set is about.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum InfoType {
    /// A file or directory.
    File,
    /// The filesystem it is on.
    FileSystem,
}

impl InfoType {
    const fn code(self) -> u8 {
        match self {
            Self::File => 0x01,
            Self::FileSystem => 0x02,
        }
    }

    const fn from_code(code: u8) -> Option<Self> {
        match code {
            0x01 => Some(Self::File),
            0x02 => Some(Self::FileSystem),
            _ => None,
        }
    }
}

/// An information class (MS-FSCC §2.4, §2.5), with the type it belongs to.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Class {
    /// `FileBasicInformation`: times and attributes.
    Basic,
    /// `FileStandardInformation`: sizes, links, deletion, kind.
    Standard,
    /// `FileInternalInformation`: the 64-bit file id.
    Internal,
    /// `FileRenameInformation`: a new name.
    Rename,
    /// `FileDispositionInformation`: delete on close.
    Disposition,
    /// `FileEndOfFileInformation`: a new size.
    EndOfFile,
    /// `FileNetworkOpenInformation`: times, sizes and attributes at once.
    NetworkOpen,
    /// `FileFsVolumeInformation`: the volume's label and serial.
    FsVolume,
    /// `FileFsAttributeInformation`: the filesystem's name and abilities.
    FsAttribute,
    /// `FileFsFullSizeInformation`: the volume's sizes.
    FsFullSize,
}

impl Class {
    const ALL: [Self; 10] = [
        Self::Basic,
        Self::Standard,
        Self::Internal,
        Self::Rename,
        Self::Disposition,
        Self::EndOfFile,
        Self::NetworkOpen,
        Self::FsVolume,
        Self::FsAttribute,
        Self::FsFullSize,
    ];

    /// The type the class belongs to and its number there: the numbers of
    /// the two types overlap.
    const fn codes(self) -> (InfoType, u8) {
        match self {
            Self::Basic => (InfoType::File, 4),
            Self::Standard => (InfoType::File, 5),
            Self::Internal => (InfoType::File, 6),
            Self::Rename => (InfoType::File, 10),
            Self::Disposition => (InfoType::File, 13),
            Self::EndOfFile => (InfoType::File, 20),
            Self::NetworkOpen => (InfoType::File, 34),
            Self::FsVolume => (InfoType::FileSystem, 1),
            Self::FsAttribute => (InfoType::FileSystem, 5),
            Self::FsFullSize => (InfoType::FileSystem, 7),
        }
    }

    fn from_codes(info_type: InfoType, code: u8) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|class| class.codes() == (info_type, code))
    }

    /// Append the type and the number.
    fn write(self, enc: &mut Encoder) {
        let (info_type, code) = self.codes();
        enc.u8(info_type.code());
        enc.u8(code);
    }

    /// The type and number next in `r`, as a class this client uses.
    fn read(r: &mut Reader<'_>) -> Result<Self, SmbError> {
        let info_type = InfoType::from_code(r.u8()?).ok_or(SmbError::Malformed)?;
        Self::from_codes(info_type, r.u8()?).ok_or(SmbError::Malformed)
    }
}

/// The most output one query may carry.
pub const MAX_INFO: usize = 4096;

/// The fixed part of a `SET_INFO` request's body, which its buffer follows.
const SET_FIXED: usize = 32;

/// A `QUERY_INFO` request.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct QueryInfoRequest {
    /// The class asked for.
    pub class: Class,
    /// The most output the server may answer with.
    pub output_length: u32,
    /// The open asked about.
    pub file_id: FileId,
}

impl QueryInfoRequest {
    /// Append the request's body after its header. An output length past
    /// [`MAX_INFO`] latches [`SmbError::TooLarge`].
    pub fn encode(&self, enc: &mut Encoder) {
        if usize::try_from(self.output_length).map_or(true, |len| len > MAX_INFO) {
            enc.fail(SmbError::TooLarge);
        }
        enc.u16(41);
        self.class.write(enc);
        enc.u32(self.output_length);
        enc.u16(0);
        enc.u16(0);
        enc.u32(0);
        enc.u32(0);
        enc.u32(0);
        enc.bytes(&self.file_id);
        enc.u8(0);
    }

    /// The request in `message`, header included.
    ///
    /// # Errors
    ///
    /// [`SmbError::Malformed`] when it is not a well-formed request for a
    /// class this client asks.
    pub fn decode(message: &[u8]) -> Result<Self, SmbError> {
        let mut r = structure(message, 41)?;
        let class = Class::read(&mut r)?;
        let output_length = r.u32()?;
        r.skip(16)?;
        Ok(Self {
            class,
            output_length,
            file_id: r.array()?,
        })
    }
}

/// A `QUERY_INFO` response: the class's bytes, borrowed from the message.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct QueryInfoResponse<'a> {
    /// The output.
    pub output: &'a [u8],
}

impl<'a> QueryInfoResponse<'a> {
    /// Append the response's body after its header.
    pub fn encode(&self, enc: &mut Encoder) {
        write_output_buffer(enc, self.output);
    }

    /// The response in `message`, header included.
    ///
    /// # Errors
    ///
    /// [`SmbError::Malformed`] when it is not a well-formed response,
    /// [`SmbError::TooLarge`] past [`MAX_INFO`].
    pub fn decode(message: &'a [u8]) -> Result<Self, SmbError> {
        Ok(Self {
            output: output_buffer(message, 9, MAX_INFO)?,
        })
    }
}

/// A `SET_INFO` request: the class and the bytes it is set to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SetInfoRequest {
    /// The class set.
    pub class: Class,
    /// The open set.
    pub file_id: FileId,
    /// The class's bytes, written by one of this module's encoders.
    pub buffer: Vec<u8>,
}

impl SetInfoRequest {
    /// The request setting `class` on `file_id` to what `write` encodes.
    ///
    /// # Errors
    ///
    /// What `write` latched: [`SmbError::TimeOutOfRange`] for a time
    /// FILETIME cannot carry exactly, [`SmbError::OutOfMemory`].
    pub fn new(
        class: Class,
        file_id: FileId,
        write: impl FnOnce(&mut Encoder),
    ) -> Result<Self, SmbError> {
        let mut enc = Encoder::new();
        write(&mut enc);
        Ok(Self {
            class,
            file_id,
            buffer: enc.finish()?,
        })
    }

    /// Append the request's body after its header.
    pub fn encode(&self, enc: &mut Encoder) {
        enc.u16(33);
        self.class.write(enc);
        let len = enc.u32_of(self.buffer.len());
        enc.u32(len);
        let offset = enc.u16_of(HEADER_LEN + SET_FIXED);
        enc.u16(offset);
        enc.u16(0);
        enc.u32(0);
        enc.bytes(&self.file_id);
        enc.buffer(&self.buffer);
    }

    /// The request in `message`, header included.
    ///
    /// # Errors
    ///
    /// [`SmbError::Malformed`] when it is not a well-formed request for a
    /// class this client sets, [`SmbError::TooLarge`] past [`MAX_INFO`].
    pub fn decode(message: &[u8]) -> Result<Self, SmbError> {
        let mut r = structure(message, 33)?;
        let class = Class::read(&mut r)?;
        let len = usize::try_from(r.u32()?).map_err(|_| SmbError::Malformed)?;
        if len > MAX_INFO {
            return Err(SmbError::TooLarge);
        }
        let offset = usize::from(r.u16()?);
        r.skip(6)?;
        let file_id = r.array()?;
        Ok(Self {
            class,
            file_id,
            buffer: owned(buffer_at(message, offset, len)?)?,
        })
    }
}

/// A `SET_INFO` response, which says nothing beyond its status.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct SetInfoResponse;

impl SetInfoResponse {
    /// Append the response's body after its header.
    pub fn encode(self, enc: &mut Encoder) {
        enc.u16(2);
    }

    /// The response in `message`, header included.
    ///
    /// # Errors
    ///
    /// [`SmbError::Malformed`] when it is not one.
    pub fn decode(message: &[u8]) -> Result<Self, SmbError> {
        structure(message, 2).map(|_| Self)
    }
}

/// `FileBasicInformation`, as queried.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct BasicInfo {
    /// The times.
    pub times: FileTimes,
    /// The attributes.
    pub attributes: FileAttributes,
}

impl BasicInfo {
    /// The class's length.
    pub const LEN: usize = 40;

    /// The class in `output`.
    ///
    /// # Errors
    ///
    /// [`SmbError::Malformed`] at any other length.
    pub fn decode(output: &[u8]) -> Result<Self, SmbError> {
        let mut r = exactly(output, Self::LEN)?;
        Ok(Self {
            times: FileTimes::read(&mut r)?,
            attributes: FileAttributes::from_bits(r.u32()?),
        })
    }

    /// Append the class.
    pub fn encode(&self, enc: &mut Encoder) {
        self.times.write(enc);
        enc.u32(self.attributes.bits());
        enc.u32(0);
    }
}

/// `FileBasicInformation`, as set: what is left `None` is left unchanged.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct BasicUpdate {
    /// A new creation time.
    pub created: Option<Time64>,
    /// A new access time.
    pub accessed: Option<Time64>,
    /// A new write time.
    pub written: Option<Time64>,
    /// A new change time.
    pub changed: Option<Time64>,
    /// New attributes.
    pub attributes: Option<FileAttributes>,
}

impl BasicUpdate {
    /// Append the class. A time FILETIME cannot carry exactly, or one that
    /// would read as "unchanged", latches [`SmbError::TimeOutOfRange`].
    pub fn encode(&self, enc: &mut Encoder) {
        for time in [self.created, self.accessed, self.written, self.changed] {
            match time.map(filetime_exact) {
                // FILETIME zero is the wire's "leave it": the epoch itself
                // cannot be set.
                Some(Ok(0)) => enc.fail(SmbError::TimeOutOfRange),
                Some(Ok(filetime)) => enc.u64(filetime),
                Some(Err(err)) => enc.fail(err),
                None => enc.u64(0),
            }
        }
        enc.u32(self.attributes.map_or(0, FileAttributes::bits));
        enc.u32(0);
    }

    /// The class in `buffer`, zero read as unchanged.
    ///
    /// # Errors
    ///
    /// [`SmbError::Malformed`] at any other length.
    pub fn decode(buffer: &[u8]) -> Result<Self, SmbError> {
        let mut r = exactly(buffer, BasicInfo::LEN)?;
        let mut time = || -> Result<Option<Time64>, SmbError> {
            Ok(match r.u64()? {
                0 => None,
                filetime => Some(time_of_filetime(filetime)?),
            })
        };
        let (created, accessed, written, changed) = (time()?, time()?, time()?, time()?);
        let attributes = match r.u32()? {
            0 => None,
            bits => Some(FileAttributes::from_bits(bits)),
        };
        Ok(Self {
            created,
            accessed,
            written,
            changed,
            attributes,
        })
    }
}

/// `FileStandardInformation`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct StandardInfo {
    /// The bytes allocated.
    pub allocation_size: u64,
    /// The size.
    pub end_of_file: u64,
    /// How many names it has.
    pub links: u32,
    /// Whether it is to be deleted on its last close.
    pub delete_pending: bool,
    /// Whether it is a directory.
    pub directory: bool,
}

impl StandardInfo {
    /// The class's length.
    pub const LEN: usize = 24;

    /// The class in `output`.
    ///
    /// # Errors
    ///
    /// [`SmbError::Malformed`] at any other length.
    pub fn decode(output: &[u8]) -> Result<Self, SmbError> {
        let mut r = exactly(output, Self::LEN)?;
        Ok(Self {
            allocation_size: r.u64()?,
            end_of_file: r.u64()?,
            links: r.u32()?,
            delete_pending: r.u8()? != 0,
            directory: r.u8()? != 0,
        })
    }

    /// Append the class.
    pub fn encode(&self, enc: &mut Encoder) {
        enc.u64(self.allocation_size);
        enc.u64(self.end_of_file);
        enc.u32(self.links);
        enc.u8(u8::from(self.delete_pending));
        enc.u8(u8::from(self.directory));
        enc.u16(0);
    }
}

/// `FileInternalInformation`: the 64-bit file id, zero where the filesystem
/// has none.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct InternalInfo {
    /// The file id.
    pub index: u64,
}

impl InternalInfo {
    /// The class's length.
    pub const LEN: usize = 8;

    /// The class in `output`.
    ///
    /// # Errors
    ///
    /// [`SmbError::Malformed`] at any other length.
    pub fn decode(output: &[u8]) -> Result<Self, SmbError> {
        Ok(Self {
            index: exactly(output, Self::LEN)?.u64()?,
        })
    }

    /// Append the class.
    pub fn encode(&self, enc: &mut Encoder) {
        enc.u64(self.index);
    }
}

/// `FileNetworkOpenInformation`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct NetworkOpenInfo {
    /// The times.
    pub times: FileTimes,
    /// The bytes allocated.
    pub allocation_size: u64,
    /// The size.
    pub end_of_file: u64,
    /// The attributes.
    pub attributes: FileAttributes,
}

impl NetworkOpenInfo {
    /// The class's length.
    pub const LEN: usize = 56;

    /// The class in `output`.
    ///
    /// # Errors
    ///
    /// [`SmbError::Malformed`] at any other length.
    pub fn decode(output: &[u8]) -> Result<Self, SmbError> {
        let mut r = exactly(output, Self::LEN)?;
        Ok(Self {
            times: FileTimes::read(&mut r)?,
            allocation_size: r.u64()?,
            end_of_file: r.u64()?,
            attributes: FileAttributes::from_bits(r.u32()?),
        })
    }

    /// Append the class.
    pub fn encode(&self, enc: &mut Encoder) {
        self.times.write(enc);
        enc.u64(self.allocation_size);
        enc.u64(self.end_of_file);
        enc.u32(self.attributes.bits());
        enc.u32(0);
    }
}

/// `FileEndOfFileInformation`: a new size.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct EndOfFileInfo {
    /// The new size.
    pub end_of_file: u64,
}

impl EndOfFileInfo {
    /// The class's length.
    pub const LEN: usize = 8;

    /// The class in `buffer`.
    ///
    /// # Errors
    ///
    /// [`SmbError::Malformed`] at any other length.
    pub fn decode(buffer: &[u8]) -> Result<Self, SmbError> {
        Ok(Self {
            end_of_file: exactly(buffer, Self::LEN)?.u64()?,
        })
    }

    /// Append the class.
    pub fn encode(&self, enc: &mut Encoder) {
        enc.u64(self.end_of_file);
    }
}

/// `FileDispositionInformation`: whether to delete on the last close.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct DispositionInfo {
    /// Whether to delete.
    pub delete: bool,
}

impl DispositionInfo {
    /// The class's length.
    pub const LEN: usize = 1;

    /// The class in `buffer`.
    ///
    /// # Errors
    ///
    /// [`SmbError::Malformed`] at any other length.
    pub fn decode(buffer: &[u8]) -> Result<Self, SmbError> {
        Ok(Self {
            delete: exactly(buffer, Self::LEN)?.u8()? != 0,
        })
    }

    /// Append the class.
    pub fn encode(&self, enc: &mut Encoder) {
        enc.u8(u8::from(self.delete));
    }
}

/// `FileRenameInformation` for SMB2 (MS-FSCC §2.4.42.2): a new path from the
/// share's root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenameInfo {
    /// Whether to replace an object already at the new path.
    pub replace: bool,
    /// The new path.
    pub target: SharePath,
}

/// The least a rename's structure may be, padded.
const RENAME_MIN: usize = 24;

impl RenameInfo {
    /// The class in `buffer`.
    ///
    /// # Errors
    ///
    /// [`SmbError::Malformed`] when it is not a well-formed rename,
    /// [`SmbError::Name`] for a path either side refuses.
    pub fn decode(buffer: &[u8]) -> Result<Self, SmbError> {
        let mut r = Reader::new(buffer);
        let replace = r.u8()? != 0;
        r.skip(7)?;
        if r.u64()? != 0 {
            return Err(SmbError::Malformed);
        }
        let len = usize::try_from(r.u32()?).map_err(|_| SmbError::Malformed)?;
        let target = SharePath::decode(r.bytes(len)?)?;
        if target.is_root() {
            return Err(SmbError::Name);
        }
        Ok(Self { replace, target })
    }

    /// Append the class.
    pub fn encode(&self, enc: &mut Encoder) {
        let start = enc.position();
        enc.u8(u8::from(self.replace));
        enc.zeros(7);
        enc.u64(0);
        let len = enc.u32_of(self.target.utf16_len());
        enc.u32(len);
        self.target.encode(enc);
        let written = enc.position().saturating_sub(start);
        enc.zeros(RENAME_MIN.saturating_sub(written));
    }
}

/// `FileFsFullSizeInformation`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct FsFullSize {
    /// Allocation units on the volume the caller may use.
    pub total_units: u64,
    /// Free units the caller may use.
    pub caller_available_units: u64,
    /// Free units in all.
    pub available_units: u64,
    /// Sectors in a unit.
    pub sectors_per_unit: u32,
    /// Bytes in a sector.
    pub bytes_per_sector: u32,
}

impl FsFullSize {
    /// The class's length.
    pub const LEN: usize = 32;

    /// The class in `output`.
    ///
    /// # Errors
    ///
    /// [`SmbError::Malformed`] at any other length.
    pub fn decode(output: &[u8]) -> Result<Self, SmbError> {
        let mut r = exactly(output, Self::LEN)?;
        Ok(Self {
            total_units: r.u64()?,
            caller_available_units: r.u64()?,
            available_units: r.u64()?,
            sectors_per_unit: r.u32()?,
            bytes_per_sector: r.u32()?,
        })
    }

    /// Append the class.
    pub fn encode(&self, enc: &mut Encoder) {
        enc.u64(self.total_units);
        enc.u64(self.caller_available_units);
        enc.u64(self.available_units);
        enc.u32(self.sectors_per_unit);
        enc.u32(self.bytes_per_sector);
    }
}

flags! {
    /// What a filesystem can do (MS-FSCC §2.5.1).
    pub struct FsAbilities(u32) {
        /// Names are looked up case-sensitively.
        const CASE_SENSITIVE_SEARCH = 0x0000_0001;
        /// Names keep their case.
        const CASE_PRESERVED_NAMES = 0x0000_0002;
        /// Names are Unicode on disk.
        const UNICODE_ON_DISK = 0x0000_0004;
        /// Access control lists are kept and enforced.
        const PERSISTENT_ACLS = 0x0000_0008;
        /// Files may be sparse.
        const SPARSE_FILES = 0x0000_0040;
        /// Reparse points exist.
        const REPARSE_POINTS = 0x0000_0080;
        /// Files may have named streams.
        const NAMED_STREAMS = 0x0004_0000;
        /// The volume is read-only.
        const READ_ONLY_VOLUME = 0x0008_0000;
        /// Files may have several names.
        const HARD_LINKS = 0x0040_0000;
        /// Files keep extended attributes.
        const EXTENDED_ATTRIBUTES = 0x0080_0000;
    }
}

/// The longest filesystem name or volume label read, in bytes of UTF-16.
const MAX_LABEL: usize = 512;

/// `FileFsAttributeInformation`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FsAttribute {
    /// What the filesystem can do.
    pub abilities: FsAbilities,
    /// The longest name it holds, in characters.
    pub max_name_length: u32,
    /// Its name, as the server calls it.
    pub name: String,
}

impl FsAttribute {
    /// The class in `output`.
    ///
    /// # Errors
    ///
    /// [`SmbError::Malformed`] when it is not one, [`SmbError::TooLarge`]
    /// for a name past the bound.
    pub fn decode(output: &[u8]) -> Result<Self, SmbError> {
        let mut r = Reader::new(output);
        let abilities = FsAbilities::from_bits(r.u32()?);
        let max_name_length = r.u32()?;
        let len = usize::try_from(r.u32()?).map_err(|_| SmbError::Malformed)?;
        if len > MAX_LABEL {
            return Err(SmbError::TooLarge);
        }
        let name = from_utf16le(r.bytes(len)?)?;
        if r.remaining() != 0 {
            return Err(SmbError::Malformed);
        }
        Ok(Self {
            abilities,
            max_name_length,
            name,
        })
    }

    /// Append the class.
    pub fn encode(&self, enc: &mut Encoder) {
        enc.u32(self.abilities.bits());
        enc.u32(self.max_name_length);
        let len = enc.u32_of(utf16le_len(&self.name));
        enc.u32(len);
        utf16le(&self.name, enc);
    }
}

/// `FileFsVolumeInformation`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FsVolume {
    /// When the volume was made.
    pub created: Time64,
    /// Its serial number, which identifies nothing reliably.
    pub serial: u32,
    /// Its label, without the padding or terminator a server may add.
    pub label: String,
}

impl FsVolume {
    /// The class in `output`.
    ///
    /// # Errors
    ///
    /// [`SmbError::Malformed`] when it is not one, [`SmbError::TooLarge`]
    /// for a label past the bound.
    pub fn decode(output: &[u8]) -> Result<Self, SmbError> {
        let mut r = Reader::new(output);
        let created = read_filetime(&mut r)?;
        let serial = r.u32()?;
        let len = usize::try_from(r.u32()?).map_err(|_| SmbError::Malformed)?;
        if len > MAX_LABEL {
            return Err(SmbError::TooLarge);
        }
        r.skip(2)?;
        let mut label = from_utf16le(r.bytes(len)?)?;
        if r.remaining() != 0 {
            return Err(SmbError::Malformed);
        }
        label.truncate(label.trim_end_matches(['\0', ' ']).len());
        Ok(Self {
            created,
            serial,
            label,
        })
    }

    /// Append the class.
    pub fn encode(&self, enc: &mut Encoder) {
        write_filetime(enc, self.created);
        enc.u32(self.serial);
        let len = enc.u32_of(utf16le_len(&self.label));
        enc.u32(len);
        enc.u16(0);
        utf16le(&self.label, enc);
    }
}

#[cfg(test)]
#[path = "info_tests.rs"]
mod tests;
