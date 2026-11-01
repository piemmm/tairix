//! Header blocks: the root, a directory, a file, a link; and a file's
//! extension blocks, which share their layout.

use alloc::vec::Vec;

use tairix_abi::DriverError;
use tairix_fsmeta::preset::amiga::MAX_COMMENT_LEN;
use tairix_fsprobe::affs::{
    be32, put_be32, seal, sums_to_zero, DateStamp, Offsets, BOOT_BLOCKS, CHECKSUM, NAME_MAX,
    ST_FILE, ST_LINKDIR, ST_LINKFILE, ST_ROOT, ST_SOFTLINK, ST_USERDIR, TABLE, T_HEADER, T_LIST,
};

use crate::name::Name;

/// What a header block is.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Kind {
    Root,
    Directory,
    File,
    SoftLink,
    /// A hard link to a file, named by its `real_entry`.
    FileLink,
    /// A hard link to a directory, presented as a soft link to it.
    DirLink,
}

impl Kind {
    fn of(sec_type: u32) -> Option<Self> {
        Some(match sec_type {
            ST_ROOT => Self::Root,
            ST_USERDIR => Self::Directory,
            ST_FILE => Self::File,
            ST_SOFTLINK => Self::SoftLink,
            ST_LINKFILE => Self::FileLink,
            ST_LINKDIR => Self::DirLink,
            _ => return None,
        })
    }

    const fn sec_type(self) -> u32 {
        match self {
            Self::Root => ST_ROOT,
            Self::Directory => ST_USERDIR,
            Self::File => ST_FILE,
            Self::SoftLink => ST_SOFTLINK,
            Self::FileLink => ST_LINKFILE,
            Self::DirLink => ST_LINKDIR,
        }
    }

    /// Whether entries are filed in it.
    pub(crate) const fn is_directory(self) -> bool {
        matches!(self, Self::Root | Self::Directory)
    }

    /// Whether it carries the protection, comment and size fields.
    const fn has_metadata(self) -> bool {
        matches!(self, Self::Directory | Self::File | Self::SoftLink)
    }
}

/// One header block, as read and validated, or as built for writing.
pub(crate) struct Header {
    pub(crate) block: u64,
    pub(crate) kind: Kind,
    buf: Vec<u8>,
    at: Offsets,
}

impl Header {
    /// The header `buf` holds, read from `block` of a volume whose root is
    /// `root` and whose blocks number `blocks`: every fixed field as the
    /// format writes it, the checksum holding, and every pointer within the
    /// volume.
    ///
    /// # Errors
    ///
    /// [`DriverError::BadMagic`] for any of it out of shape.
    pub(crate) fn parse(
        block: u64,
        buf: Vec<u8>,
        at: Offsets,
        root: u64,
        blocks: u64,
    ) -> Result<Self, DriverError> {
        let key = u64::from(be32(&buf, 4));
        let kind = Kind::of(be32(&buf, at.sec_type())).ok_or(DriverError::BadMagic)?;
        let keyed = if kind == Kind::Root {
            block == root && key == 0
        } else {
            block != root && key == block
        };
        if be32(&buf, 0) != T_HEADER || !keyed || !sums_to_zero(&buf) {
            return Err(DriverError::BadMagic);
        }
        let header = Self {
            block,
            kind,
            buf,
            at,
        };
        let in_volume = |pointer: u32| null_or_within(pointer, blocks);
        let named = usize::from(header.buf[at.name()]) <= NAME_MAX;
        let commented =
            !kind.has_metadata() || usize::from(header.buf[at.comment()]) <= MAX_COMMENT_LEN;
        let linked = [
            header.hash_chain(),
            header.extension(),
            header.next_link(),
            header.real_entry(),
        ]
        .into_iter()
        .all(in_volume);
        let parented = kind == Kind::Root || header.parent() != 0 && in_volume(header.parent());
        if !(named && commented && linked && parented) {
            return Err(DriverError::BadMagic);
        }
        Ok(header)
    }

    /// A fresh header of `kind` for `block`, named `name`, in directory
    /// `parent`, dated `date`.
    pub(crate) fn new(
        block: u64,
        kind: Kind,
        mut buf: Vec<u8>,
        at: Offsets,
        name: &Name,
        parent: u64,
        date: DateStamp,
    ) -> Self {
        buf.fill(0);
        put_be32(&mut buf, 0, T_HEADER);
        put_be32(&mut buf, 4, low32(block));
        put_be32(&mut buf, at.sec_type(), kind.sec_type());
        let mut header = Self {
            block,
            kind,
            buf,
            at,
        };
        header.set_name(name);
        header.set_parent(parent);
        header.set_date(date);
        header
    }

    /// The block's bytes, sealed: what is written.
    pub(crate) fn sealed(&mut self) -> &[u8] {
        seal(&mut self.buf, CHECKSUM);
        &self.buf
    }

    /// The block's buffer, given back for another read.
    pub(crate) fn into_buf(self) -> Vec<u8> {
        self.buf
    }

    /// Take `other`'s bytes: the same block, as a later write left it.
    pub(crate) fn copy_from(&mut self, other: &Self) {
        let len = self.buf.len().min(other.buf.len());
        self.buf[..len].copy_from_slice(&other.buf[..len]);
    }

    fn long(&self, at: usize) -> u32 {
        be32(&self.buf, at)
    }

    fn set_long(&mut self, at: usize, value: u32) {
        put_be32(&mut self.buf, at, value);
    }

    /// Entries of the hash table or the data block pointer table.
    pub(crate) fn table_len(&self) -> usize {
        self.at.table_len()
    }

    /// Slot `index` of the hash table, or of the data block pointers, which a
    /// file fills from the table's end.
    pub(crate) fn table(&self, index: usize) -> u32 {
        self.long(TABLE + 4 * index)
    }

    pub(crate) fn set_table(&mut self, index: usize, value: u32) {
        self.set_long(TABLE + 4 * index, value);
    }

    /// Data block pointers a file header or an extension block holds.
    pub(crate) fn high_seq(&self) -> u32 {
        self.long(8)
    }

    pub(crate) fn set_high_seq(&mut self, count: u32) {
        self.set_long(8, count);
    }

    /// Set a file's first data block.
    pub(crate) fn set_first_data(&mut self, block: u32) {
        self.set_long(16, block);
    }

    /// The protection bits.
    pub(crate) fn protect(&self) -> u32 {
        self.long(self.at.protect())
    }

    pub(crate) fn set_protect(&mut self, bits: u32) {
        let at = self.at.protect();
        self.set_long(at, bits);
    }

    /// The protection bits' low byte, `hsparwed`, as recorded: the owner's
    /// `rwed` are set to deny.
    pub(crate) fn protection_byte(&self) -> u8 {
        self.protect().to_le_bytes()[0]
    }

    /// The root's creation date.
    pub(crate) fn created(&self) -> DateStamp {
        DateStamp::read(&self.buf, self.at.created())
    }

    /// Stamp the root with the volume's last alteration.
    pub(crate) fn set_volume_date(&mut self, date: DateStamp) {
        let at = self.at.volume_date();
        date.write(&mut self.buf, at);
    }

    /// A file's length in bytes.
    pub(crate) fn byte_size(&self) -> u32 {
        self.long(self.at.byte_size())
    }

    pub(crate) fn set_byte_size(&mut self, size: u32) {
        let at = self.at.byte_size();
        self.set_long(at, size);
    }

    /// The comment.
    pub(crate) fn comment(&self) -> &[u8] {
        let at = self.at.comment();
        &self.buf[at + 1..at + 1 + usize::from(self.buf[at])]
    }

    /// Set the comment, at most [`MAX_COMMENT_LEN`] bytes.
    pub(crate) fn set_comment(&mut self, comment: &[u8]) {
        let at = self.at.comment();
        self.buf[at..=at + MAX_COMMENT_LEN].fill(0);
        self.buf[at] = u8::try_from(comment.len()).unwrap_or(0);
        self.buf[at + 1..at + 1 + comment.len()].copy_from_slice(comment);
    }

    /// The date stamp: a header's last change, the root's last alteration.
    pub(crate) fn date(&self) -> DateStamp {
        DateStamp::read(&self.buf, self.at.date())
    }

    pub(crate) fn set_date(&mut self, date: DateStamp) {
        let at = self.at.date();
        date.write(&mut self.buf, at);
    }

    /// The name.
    pub(crate) fn name(&self) -> Result<Name, DriverError> {
        let at = self.at.name();
        Name::recorded(&self.buf[at + 1..at + 1 + usize::from(self.buf[at])])
    }

    pub(crate) fn set_name(&mut self, name: &Name) {
        let at = self.at.name();
        self.buf[at..=at + NAME_MAX].fill(0);
        self.buf[at] = u8::try_from(name.as_bytes().len()).unwrap_or(0);
        self.buf[at + 1..at + 1 + name.as_bytes().len()].copy_from_slice(name.as_bytes());
    }

    /// The next header in its hash chain.
    pub(crate) fn hash_chain(&self) -> u32 {
        self.long(self.at.hash_chain())
    }

    pub(crate) fn set_hash_chain(&mut self, next: u32) {
        let at = self.at.hash_chain();
        self.set_long(at, next);
    }

    /// The directory it is filed in.
    pub(crate) fn parent(&self) -> u32 {
        self.long(self.at.parent())
    }

    pub(crate) fn set_parent(&mut self, dir: u64) {
        let at = self.at.parent();
        self.set_long(at, low32(dir));
    }

    /// A file's first extension block.
    pub(crate) fn extension(&self) -> u32 {
        self.long(self.at.extension())
    }

    pub(crate) fn set_extension(&mut self, block: u32) {
        let at = self.at.extension();
        self.set_long(at, block);
    }

    /// The object a hard link names.
    pub(crate) fn real_entry(&self) -> u32 {
        if matches!(self.kind, Kind::FileLink | Kind::DirLink) {
            self.long(self.at.real_entry())
        } else {
            0
        }
    }

    pub(crate) fn set_real_entry(&mut self, block: u64) {
        let at = self.at.real_entry();
        self.set_long(at, low32(block));
    }

    /// The newest hard link naming this object, or the next link after this
    /// one.
    pub(crate) fn next_link(&self) -> u32 {
        if self.kind == Kind::Root {
            0
        } else {
            self.long(self.at.next_link())
        }
    }

    pub(crate) fn set_next_link(&mut self, block: u32) {
        let at = self.at.next_link();
        self.set_long(at, block);
    }

    /// A soft link's stored target, up to its terminator.
    pub(crate) fn link_target(&self) -> &[u8] {
        let room = &self.buf[TABLE..self.at.bm_flag()];
        let end = room.iter().position(|&b| b == 0).unwrap_or(room.len());
        &room[..end]
    }

    /// Store a soft link's target, at most [`Self::link_room`] bytes.
    pub(crate) fn set_link_target(&mut self, target: &[u8]) {
        let end = self.at.bm_flag();
        self.buf[TABLE..end].fill(0);
        self.buf[TABLE..TABLE + target.len()].copy_from_slice(target);
    }

    /// The bytes a soft link's target may take, its terminator left room.
    pub(crate) fn link_room(at: Offsets) -> usize {
        at.bm_flag() - TABLE - 1
    }

    /// The root's bitmap flag, its bitmap block pointers following.
    pub(crate) fn long_at(&self, at: usize) -> u32 {
        self.long(at)
    }

    pub(crate) fn set_long_at(&mut self, at: usize, value: u32) {
        self.set_long(at, value);
    }
}

/// A file extension block: more data block pointers, chained from the
/// header.
pub(crate) struct Extension {
    pub(crate) block: u64,
    buf: Vec<u8>,
    at: Offsets,
}

impl Extension {
    /// The extension `buf` holds, read from `block`, for `file`: its type,
    /// its key, its owner and its checksum holding, and its pointers within
    /// the volume.
    ///
    /// # Errors
    ///
    /// [`DriverError::BadMagic`] for any of it out of shape.
    pub(crate) fn parse(
        block: u64,
        buf: Vec<u8>,
        at: Offsets,
        file: u64,
        blocks: u64,
    ) -> Result<Self, DriverError> {
        let ext = Self { block, buf, at };
        let shaped = be32(&ext.buf, 0) == T_LIST
            && u64::from(be32(&ext.buf, 4)) == block
            && u64::from(ext.owner()) == file
            && be32(&ext.buf, at.sec_type()) == ST_FILE
            && usize::try_from(ext.high_seq()).is_ok_and(|n| n <= at.table_len())
            && null_or_within(ext.next(), blocks)
            && sums_to_zero(&ext.buf);
        if !shaped {
            return Err(DriverError::BadMagic);
        }
        Ok(ext)
    }

    /// A fresh extension block for `file` at `block`.
    pub(crate) fn new(block: u64, mut buf: Vec<u8>, at: Offsets, file: u64) -> Self {
        buf.fill(0);
        put_be32(&mut buf, 0, T_LIST);
        put_be32(&mut buf, 4, low32(block));
        put_be32(&mut buf, at.parent(), low32(file));
        put_be32(&mut buf, at.sec_type(), ST_FILE);
        Self { block, buf, at }
    }

    pub(crate) fn sealed(&mut self) -> &[u8] {
        seal(&mut self.buf, CHECKSUM);
        &self.buf
    }

    /// The block's buffer, given back for another read.
    pub(crate) fn into_buf(self) -> Vec<u8> {
        self.buf
    }

    pub(crate) fn table(&self, index: usize) -> u32 {
        be32(&self.buf, TABLE + 4 * index)
    }

    pub(crate) fn set_table(&mut self, index: usize, value: u32) {
        put_be32(&mut self.buf, TABLE + 4 * index, value);
    }

    pub(crate) fn high_seq(&self) -> u32 {
        be32(&self.buf, 8)
    }

    pub(crate) fn set_high_seq(&mut self, count: u32) {
        put_be32(&mut self.buf, 8, count);
    }

    fn owner(&self) -> u32 {
        be32(&self.buf, self.at.parent())
    }

    /// The next extension block.
    pub(crate) fn next(&self) -> u32 {
        be32(&self.buf, self.at.extension())
    }

    pub(crate) fn set_next(&mut self, block: u32) {
        put_be32(&mut self.buf, self.at.extension(), block);
    }
}

/// Whether `block` is one of a volume of `blocks` blocks a structure may name:
/// past the boot blocks, within the volume.
pub(crate) const fn within(block: u64, blocks: u64) -> bool {
    block >= BOOT_BLOCKS && block < blocks
}

/// Whether `pointer`, a link a structure records, names nothing or a block
/// [`within`] a volume of `blocks` blocks.
fn null_or_within(pointer: u32, blocks: u64) -> bool {
    pointer == 0 || within(u64::from(pointer), blocks)
}

/// A block number as the format records it: every pointer is a longword, and
/// a volume has no more blocks than one counts.
pub(crate) fn low32(block: u64) -> u32 {
    u32::try_from(block).unwrap_or(0)
}
