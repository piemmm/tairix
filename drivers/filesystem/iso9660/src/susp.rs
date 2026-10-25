//! A directory record's system use: the RISC OS field or Apple record that
//! may open it, and the System Use Sharing Protocol entries that follow —
//! Rock Ridge's POSIX records (RRIP 1.12, IEEE P1282), zisofs compression,
//! and Apple's and the Amiga's records among them.

use alloc::vec::Vec;

use tairix_abi::fs::{FS_NAME_MAX, FS_SYMLINK_MAX};
use tairix_abi::time::Time64;
use tairix_abi::DriverError;
use tairix_fsmeta::preset::acorn;
use tairix_fsmeta::preset::mac::FinderInfo;
use tairix_fsprobe::iso9660::{both_u32, ARCHIMEDES, ARCHIMEDES_LEN};

use crate::time;

/// Bytes of a SUSP entry's signature, length and version.
const ENTRY_HEADER: usize = 4;

/// RISC OS attribute flag: the name's first character stands for `!`, which
/// no d-character spells.
const ARCHIMEDES_BANG: u32 = 0x100;

/// POSIX file type bits of a Rock Ridge mode.
pub(crate) const S_IFMT: u32 = 0o170_000;
/// POSIX file type: directory.
pub(crate) const S_IFDIR: u32 = 0o040_000;
/// POSIX file type: regular file.
pub(crate) const S_IFREG: u32 = 0o100_000;
/// POSIX file type: symbolic link.
pub(crate) const S_IFLNK: u32 = 0o120_000;

/// A Rock Ridge POSIX record (`PX`).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Posix {
    pub mode: u32,
    pub links: u32,
    pub uid: u32,
    pub gid: u32,
}

/// A zisofs compressed file's `ZF` record: its blocks' size and its
/// uncompressed length.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Zisofs {
    /// Bytes of the file's header, in four-byte words.
    pub header_words: u8,
    /// Log2 of the bytes of one uncompressed block.
    pub log2_block: u8,
    /// The file's uncompressed length.
    pub size: u32,
}

/// Where a record's system use continues (`CE`).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Continuation {
    pub block: u32,
    pub offset: u32,
    pub len: u32,
}

/// The instants a Rock Ridge `TF` record keeps.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Stamps {
    pub created: Option<Time64>,
    pub modified: Option<Time64>,
    pub accessed: Option<Time64>,
    pub changed: Option<Time64>,
}

/// Whether a file is a Rock Ridge sparse file, and the size its `SF` entry
/// records.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Sparse {
    No,
    Sized(u64),
    /// An `SF` entry whose size does not decode.
    Malformed,
}

impl Sparse {
    /// The `SF` entry `entry`: its virtual size's high then low half, each
    /// recorded both-endian.
    fn of(entry: &[u8]) -> Self {
        match (both_u32(entry, 4), both_u32(entry, 12)) {
            (Some(high), Some(low)) => Self::Sized(u64::from(high) << 32 | u64::from(low)),
            _ => Self::Malformed,
        }
    }
}

/// A name assembled from `NM` entries.
pub(crate) struct AlternateName {
    bytes: [u8; FS_NAME_MAX],
    len: usize,
    /// The last entry said the name continues.
    continuing: bool,
    /// The name outgrew [`FS_NAME_MAX`]: the tree's own name stands instead.
    overlong: bool,
}

impl AlternateName {
    const fn new() -> Self {
        Self {
            bytes: [0; FS_NAME_MAX],
            len: 0,
            continuing: false,
            overlong: false,
        }
    }

    /// The name, unless it outgrew the bound.
    pub(crate) fn get(&self) -> Option<&[u8]> {
        (!self.overlong).then_some(&self.bytes[..self.len])
    }

    fn push(&mut self, part: &[u8], continues: bool) {
        match self.bytes.get_mut(self.len..self.len + part.len()) {
            Some(slot) if !self.overlong => {
                slot.copy_from_slice(part);
                self.len += part.len();
            }
            _ => self.overlong = true,
        }
        self.continuing = continues;
    }
}

/// A symbolic link target assembled from `SL` entries.
pub(crate) struct Target {
    bytes: Vec<u8>,
    /// The last component said it continues, so the next joins it unbroken.
    glued: bool,
}

impl Target {
    /// The target's bytes.
    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    fn push(&mut self, part: &[u8]) -> Result<(), DriverError> {
        if self.bytes.len() + part.len() > FS_SYMLINK_MAX {
            return Err(DriverError::DeviceFault);
        }
        if !tairix_util::fallible::reserve(&mut self.bytes, part.len()) {
            return Err(DriverError::OutOfMemory);
        }
        self.bytes.extend_from_slice(part);
        Ok(())
    }
}

/// The Amiga record (`AS`, Rock Ridge Amiga extensions revision 2.4).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Amiga {
    /// The low byte of the `AmigaOS` protection word: `hsparwed`.
    pub protection: Option<u8>,
    pub comment: Vec<u8>,
    /// No later `AS` entry adds to the comment.
    comment_complete: bool,
}

/// What a record's system use declares.
pub(crate) struct SystemUse {
    pub posix: Option<Posix>,
    pub name: Option<AlternateName>,
    pub link: Option<Target>,
    pub stamps: Stamps,
    /// The relocated directory this record stands for (`CL`).
    pub child: Option<u32>,
    /// This record is a relocated directory, reached only through its `CL`.
    pub relocated: bool,
    /// The directory a relocated directory belongs in (`PL`), which its `..`
    /// record carries.
    pub parent: Option<u32>,
    /// A sparse file (`SF`), whose layout is not read.
    pub sparse: Sparse,
    pub zisofs: Option<Zisofs>,
    /// RISC OS's load and execution words and attributes, the directory bit
    /// set from the record.
    pub acorn: Option<acorn::Object>,
    /// The RISC OS name starts with the `!` its first character stands for.
    pub bang: bool,
    /// Apple's Finder information (Apple Technical Note FL36).
    pub finder: Option<FinderInfo>,
    pub amiga: Option<Amiga>,
}

impl SystemUse {
    pub(crate) const fn new() -> Self {
        Self {
            posix: None,
            name: None,
            link: None,
            stamps: Stamps {
                created: None,
                modified: None,
                accessed: None,
                changed: None,
            },
            child: None,
            relocated: false,
            parent: None,
            sparse: Sparse::No,
            zisofs: None,
            acorn: None,
            bang: false,
            finder: None,
            amiga: None,
        }
    }

    /// Read the Apple record a disc without SUSP opens a record's system use
    /// with: at offset 0, or past a CD-ROM XA record's fourteen bytes.
    pub(crate) fn read_apple(&mut self, field: &[u8]) {
        let at = if field.get(6..8) == Some(b"XA") {
            14
        } else {
            0
        };
        let Some(entry) = field.get(at..) else {
            return;
        };
        if entry.starts_with(b"AA") {
            self.read_aa(entry);
        } else if entry.starts_with(b"BA") {
            // The original form records no length: its ID implies one.
            self.finder = match entry.get(2..) {
                Some([6, rest @ ..]) => hfs(rest.get(..10)),
                Some([1, rest @ ..]) => prodos(rest.get(..3)),
                _ => None,
            };
        }
    }

    /// `AA`: a length, then an ID naming HFS (2) or `ProDOS` (1) information.
    /// libisofs once wrote entries of its own under the signature, at other
    /// lengths, which are not Apple's.
    fn read_aa(&mut self, entry: &[u8]) {
        self.finder = match (entry.get(2), entry.get(3)) {
            (Some(14), Some(2)) => hfs(entry.get(4..14)),
            (Some(7), Some(1)) => prodos(entry.get(4..7)),
            _ => self.finder,
        };
    }

    /// `AS`: flags, then the protection word when flagged, then a comment
    /// whose length counts itself and which may continue in the next `AS`.
    /// A comment longer than `AmigaOS` keeps is damage and is dropped whole,
    /// never cut short.
    fn read_amiga(&mut self, entry: &[u8]) {
        const PROTECTION: u8 = 1;
        const COMMENT: u8 = 2;
        const CONTINUE: u8 = 4;
        let Some(&flags) = entry.get(4) else {
            return;
        };
        let amiga = self.amiga.get_or_insert_with(Amiga::default);
        let mut at = 5;
        if flags & PROTECTION != 0 {
            let Some(word) = entry.get(at..at + 4) else {
                return;
            };
            amiga.protection = Some(word[3]);
            at += 4;
        }
        if flags & COMMENT == 0 || amiga.comment_complete {
            return;
        }
        let len = usize::from(entry.get(at).copied().unwrap_or(0));
        let text = entry.get(at + 1..at + len.max(1));
        let kept = text.filter(|text| {
            amiga.comment.len() + text.len() <= tairix_fsmeta::preset::amiga::MAX_COMMENT_LEN
                && tairix_util::fallible::reserve(&mut amiga.comment, text.len())
        });
        match kept {
            Some(text) => amiga.comment.extend_from_slice(text),
            None => amiga.comment.clear(),
        }
        amiga.comment_complete = kept.is_none() || flags & CONTINUE == 0;
    }

    /// Read the RISC OS field opening `field`, the system use of a record
    /// that `is_dir`.
    pub(crate) fn read_archimedes(&mut self, field: &[u8], is_dir: bool) {
        let Some(raw) = field.get(..ARCHIMEDES_LEN) else {
            return;
        };
        if !raw.starts_with(ARCHIMEDES) {
            return;
        }
        let word = |at: usize| u32::from_le_bytes([raw[at], raw[at + 1], raw[at + 2], raw[at + 3]]);
        let attributes = word(18);
        let directory = if is_dir { acorn::ATTR_DIRECTORY } else { 0 };
        self.acorn = Some(acorn::Object {
            load: word(10),
            exec: word(14),
            attr: acorn::attr_from_fileswitch(attributes.to_le_bytes()[0]) | directory,
        });
        self.bang = attributes & ARCHIMEDES_BANG != 0;
    }

    /// Read the SUSP entries in `area`, answering where they continue. An
    /// entry whose length runs past the area ends it, as the padding a
    /// record's system use may end in does; an `ST` ends the whole use.
    pub(crate) fn read_entries(&mut self, area: &[u8]) -> Result<Next, DriverError> {
        let mut next = Next::End;
        let mut pos = 0;
        while let Some(header) = area.get(pos..pos + ENTRY_HEADER) {
            let len = usize::from(header[2]);
            let Some(entry) = area.get(pos..pos + len).filter(|_| len >= ENTRY_HEADER) else {
                break;
            };
            match &entry[..2] {
                b"CE" => {
                    if let (Some(block), Some(offset), Some(len)) =
                        (both_u32(entry, 4), both_u32(entry, 12), both_u32(entry, 20))
                    {
                        next = Next::Continue(Continuation { block, offset, len });
                    }
                }
                b"PX" => {
                    if let (Some(mode), Some(links), Some(uid), Some(gid)) = (
                        both_u32(entry, 4),
                        both_u32(entry, 12),
                        both_u32(entry, 20),
                        both_u32(entry, 28),
                    ) {
                        self.posix = Some(Posix {
                            mode,
                            links,
                            uid,
                            gid,
                        });
                    }
                }
                b"NM" => self.read_name(entry),
                b"SL" => self.read_link(entry)?,
                b"CL" => self.child = both_u32(entry, 4),
                b"PL" => self.parent = both_u32(entry, 4),
                b"RE" => self.relocated = true,
                b"SF" => self.sparse = Sparse::of(entry),
                b"TF" => self.read_stamps(entry),
                b"ZF" => self.read_zisofs(entry),
                b"AA" => self.read_aa(entry),
                b"AS" => self.read_amiga(entry),
                b"ST" => return Ok(Next::Stop),
                _ => {}
            }
            pos += len;
        }
        Ok(next)
    }

    /// `NM`: flags, then the name's bytes. The current and parent names
    /// belong to the `.` and `..` records, which are not listed.
    fn read_name(&mut self, entry: &[u8]) {
        const CONTINUE: u8 = 1;
        const CURRENT_OR_PARENT: u8 = 2 | 4;
        let Some(&flags) = entry.get(4) else {
            return;
        };
        if flags & CURRENT_OR_PARENT != 0 {
            return;
        }
        let name = self.name.get_or_insert_with(AlternateName::new);
        // A name already complete is not continued by a later entry.
        if name.len != 0 && !name.continuing {
            return;
        }
        name.push(&entry[5..], flags & CONTINUE != 0);
    }

    /// `SL`: flags, then component records — flags, length, content — that
    /// join into a path, a component continuing into the next unbroken.
    fn read_link(&mut self, entry: &[u8]) -> Result<(), DriverError> {
        const CONTINUE: u8 = 1;
        const CURRENT: u8 = 2;
        const PARENT: u8 = 4;
        const ROOT: u8 = 8;
        let target = match &mut self.link {
            Some(target) => target,
            empty => empty.insert(Target {
                bytes: Vec::new(),
                glued: false,
            }),
        };
        let mut pos = 5;
        while let Some(&[flags, len]) = entry.get(pos..pos + 2) {
            let Some(content) = entry.get(pos + 2..pos + 2 + usize::from(len)) else {
                return Err(DriverError::DeviceFault);
            };
            if flags & ROOT != 0 {
                if target.bytes.is_empty() {
                    target.push(b"/")?;
                }
                target.glued = true;
            } else {
                if !target.bytes.is_empty() && !target.glued {
                    target.push(b"/")?;
                }
                let part: &[u8] = if flags & CURRENT != 0 {
                    b"."
                } else if flags & PARENT != 0 {
                    b".."
                } else {
                    content
                };
                target.push(part)?;
                target.glued = flags & CONTINUE != 0;
            }
            pos += 2 + usize::from(len);
        }
        Ok(())
    }

    /// `TF`: flags naming which instants follow, each seven bytes or, under
    /// the long-form flag, seventeen.
    fn read_stamps(&mut self, entry: &[u8]) {
        const LONG_FORM: u8 = 0x80;
        let Some(&flags) = entry.get(4) else {
            return;
        };
        let width = if flags & LONG_FORM != 0 { 17 } else { 7 };
        let mut at = 5;
        for bit in 0..7 {
            if flags & (1 << bit) == 0 {
                continue;
            }
            let Some(raw) = entry.get(at..at + width) else {
                return;
            };
            at += width;
            let instant = match <&[u8; 17]>::try_from(raw) {
                Ok(long) => time::long_date(long),
                Err(_) => <[u8; 7]>::try_from(raw).ok().and_then(time::record_date),
            };
            match bit {
                0 => self.stamps.created = instant,
                1 => self.stamps.modified = instant,
                2 => self.stamps.accessed = instant,
                3 => self.stamps.changed = instant,
                _ => {}
            }
        }
    }

    /// `ZF`: the algorithm, the header's size in words, the block size's
    /// log2 and the uncompressed length. Only zisofs's `pz` is known.
    fn read_zisofs(&mut self, entry: &[u8]) {
        if entry.get(4..6) != Some(b"pz") {
            return;
        }
        let (Some(&header_words), Some(&log2_block), Some(size)) =
            (entry.get(6), entry.get(7), both_u32(entry, 8))
        else {
            return;
        };
        self.zisofs = Some(Zisofs {
            header_words,
            log2_block,
            size,
        });
    }
}

/// Where a record's system use goes after one area.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Next {
    /// The use ends with this area.
    End,
    /// An `ST` ended it.
    Stop,
    /// It continues in another area.
    Continue(Continuation),
}

/// HFS Finder information: type, creator and flags, big-endian.
fn hfs(raw: Option<&[u8]>) -> Option<FinderInfo> {
    let raw = raw?;
    Some(FinderInfo {
        file_type: raw.get(..4)?.try_into().ok()?,
        creator: raw.get(4..8)?.try_into().ok()?,
        flags: u16::from_be_bytes([*raw.get(8)?, *raw.get(9)?]),
    })
}

/// `ProDOS` information, as Apple names a `ProDOS` file to HFS: type `p` with the
/// file type and the auxiliary type, creator `pdos`.
fn prodos(raw: Option<&[u8]>) -> Option<FinderInfo> {
    let raw = raw?;
    let aux = u16::from_le_bytes([*raw.get(1)?, *raw.get(2)?]).to_be_bytes();
    Some(FinderInfo {
        file_type: [b'p', *raw.first()?, aux[0], aux[1]],
        creator: *b"pdos",
        flags: 0,
    })
}
