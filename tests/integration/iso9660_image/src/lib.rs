//! ISO 9660 discs mastered for tests: the directory records, volume
//! descriptors and system use entries a disc is made of, and whole trees laid
//! out into an image. The ISO 9660 driver's tests and fuzz seeds, the volume
//! probe's tests and the QEMU vertical that copies a RISC OS disc onto ARXFS
//! ([`riscos_disc`]) all master their discs here.
//!
//! Each encoder follows its specification — ECMA-119, the High Sierra
//! standard, SUSP and Rock Ridge, Apple Technical Note FL36, the RISC OS
//! `ARCHIMEDES` field — and none borrows from the code whose reading it
//! checks. 2048-byte logical blocks throughout.

#![no_std]
#![deny(missing_docs)]
// Fixture images are a few MiB, so every offset and length fits the narrower
// on-disk field it is written to.
#![allow(clippy::cast_possible_truncation)]

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;

/// Bytes of a logical block and sector.
pub const BLOCK: usize = 2048;

/// Directory record flag: a directory.
pub const DIRECTORY: u8 = 2;
/// Directory record flag: an associated file.
pub const ASSOCIATED: u8 = 4;
/// Directory record flag: owner, group and permissions are in the XAR.
pub const PROTECTION: u8 = 16;
/// Directory record flag: another record of the same file follows.
pub const MULTI_EXTENT: u8 = 128;

/// The standard a disc is recorded to.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum Standard {
    /// ECMA-119.
    #[default]
    Iso9660,
    /// The High Sierra standard ECMA-119 grew from: the same structures with
    /// their fields placed apart.
    HighSierra,
}

/// One node of a tree to master.
pub struct Node {
    /// The identifier as recorded: `b"NAME.EXT;1"`, or UCS-2 for Joliet.
    pub identifier: Vec<u8>,
    /// The record's system use field.
    pub system_use: Vec<u8>,
    /// Flags beyond the directory bit.
    pub flags: u8,
    /// The recording date's seven bytes; High Sierra records the first six.
    pub date: [u8; 7],
    /// What the node holds.
    pub kind: Kind,
}

/// What a node holds.
pub enum Kind {
    /// A file's bytes, split into one record per part when `parts` names
    /// their lengths, with an extended attribute record and interleaving
    /// when asked for.
    File {
        /// The file's bytes.
        data: Vec<u8>,
        /// The length of each multi-extent part, in order.
        parts: Option<Vec<usize>>,
        /// The extended attribute record ahead of the data.
        xar: Option<Vec<u8>>,
        /// File unit and interleave gap sizes, in blocks.
        interleave: Option<(u8, u8)>,
    },
    /// A directory, its `.` record carrying `dot_use`.
    Dir {
        /// The directory's children, in recording order.
        children: Vec<Node>,
        /// The system use of the directory's `.` record.
        dot_use: Vec<u8>,
    },
    /// A record naming a given extent, as a Rock Ridge `CL` record does.
    Pointer {
        /// The extent the record names.
        extent: u32,
        /// The data length it records.
        len: u32,
    },
}

/// The recording date every node is mastered with: 2021-06-15 12:30:00 UTC.
const DATE: [u8; 7] = [121, 6, 15, 12, 30, 0, 0];

impl Node {
    /// A file named `identifier` holding `data`.
    #[must_use]
    pub fn file(identifier: &[u8], data: &[u8]) -> Self {
        Self {
            identifier: identifier.to_vec(),
            system_use: Vec::new(),
            flags: 0,
            date: DATE,
            kind: Kind::File {
                data: data.to_vec(),
                parts: None,
                xar: None,
                interleave: None,
            },
        }
    }

    /// A directory named `identifier`.
    #[must_use]
    pub fn dir(identifier: &[u8], children: Vec<Node>) -> Self {
        Self {
            identifier: identifier.to_vec(),
            system_use: Vec::new(),
            flags: 0,
            date: DATE,
            kind: Kind::Dir {
                children,
                dot_use: Vec::new(),
            },
        }
    }

    /// This node with `system_use`.
    #[must_use]
    pub fn with_use(mut self, system_use: Vec<u8>) -> Self {
        self.system_use = system_use;
        self
    }

    /// This node with `flags`.
    #[must_use]
    pub fn with_flags(mut self, flags: u8) -> Self {
        self.flags |= flags;
        self
    }
}

/// What a tree to master is.
pub struct Disc {
    /// The primary descriptor's volume identifier.
    pub volume_id: Vec<u8>,
    /// The primary tree.
    pub root: Node,
    /// A Joliet tree, its identifiers UCS-2. Joliet is ISO 9660's alone, so
    /// a High Sierra disc records none.
    pub joliet: Option<Node>,
    /// The standard the disc is recorded to.
    pub standard: Standard,
}

impl Default for Disc {
    fn default() -> Self {
        Self {
            volume_id: Vec::new(),
            root: Node::dir(&[], Vec::new()),
            joliet: None,
            standard: Standard::Iso9660,
        }
    }
}

/// A mastered image and where its directories landed, by their order in a
/// depth-first walk from the root.
pub struct Mastered {
    /// The image's bytes.
    pub image: Vec<u8>,
    /// Each directory's first block, depth first from the root.
    pub directories: Vec<u32>,
}

/// UCS-2 big-endian bytes of `text`.
#[must_use]
pub fn ucs2(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_be_bytes).collect()
}

/// A both-endian `u32`.
#[must_use]
pub fn both32(value: u32) -> [u8; 8] {
    let mut out = [0u8; 8];
    out[..4].copy_from_slice(&value.to_le_bytes());
    out[4..].copy_from_slice(&value.to_be_bytes());
    out
}

/// A both-endian `u16`.
#[must_use]
pub fn both16(value: u16) -> [u8; 4] {
    let mut out = [0u8; 4];
    out[..2].copy_from_slice(&value.to_le_bytes());
    out[2..].copy_from_slice(&value.to_be_bytes());
    out
}

/// The fields of one directory record.
#[derive(Clone, Copy)]
pub struct Rec<'a> {
    /// The first block of the record's extent.
    pub extent: u32,
    /// The data length it records.
    pub len: u32,
    /// The record's flags.
    pub flags: u8,
    /// The recording date's seven bytes.
    pub date: [u8; 7],
    /// The identifier as recorded.
    pub name: &'a [u8],
    /// The system use field.
    pub system_use: &'a [u8],
    /// File unit and interleave gap sizes, in blocks.
    pub interleave: (u8, u8),
    /// Blocks of extended attribute record ahead of the data.
    pub xar_blocks: u8,
    /// The standard whose layout the record takes.
    pub standard: Standard,
}

impl<'a> Rec<'a> {
    /// An ECMA-119 record of `name` for `len` bytes at `extent`.
    #[must_use]
    pub fn new(extent: u32, len: u32, flags: u8, name: &'a [u8]) -> Self {
        Self {
            extent,
            len,
            flags,
            date: DATE,
            name,
            system_use: &[],
            interleave: (0, 0),
            xar_blocks: 0,
            standard: Standard::Iso9660,
        }
    }

    /// The record's bytes.
    ///
    /// # Panics
    ///
    /// For a record longer than its one-byte length can say.
    #[must_use]
    pub fn bytes(&self) -> Vec<u8> {
        let name = self.name;
        let pad = usize::from(name.len().is_multiple_of(2));
        let total = 33 + name.len() + pad + self.system_use.len();
        assert!(total <= 255, "a record fits its length byte");
        let mut r = vec![0u8; total];
        r[0] = total as u8;
        r[1] = self.xar_blocks;
        r[2..10].copy_from_slice(&both32(self.extent));
        r[10..18].copy_from_slice(&both32(self.len));
        match self.standard {
            Standard::Iso9660 => {
                r[18..25].copy_from_slice(&self.date);
                r[25] = self.flags;
            }
            // Six bytes of date, no offset from UTC, then the flags.
            Standard::HighSierra => {
                r[18..24].copy_from_slice(&self.date[..6]);
                r[24] = self.flags;
            }
        }
        r[26] = self.interleave.0;
        r[27] = self.interleave.1;
        r[28..32].copy_from_slice(&both16(1));
        r[32] = name.len() as u8;
        r[33..33 + name.len()].copy_from_slice(name);
        r[33 + name.len() + pad..].copy_from_slice(self.system_use);
        r
    }
}

/// A primary or supplementary volume descriptor.
pub struct Descriptor<'a> {
    /// The standard whose layout it takes.
    pub standard: Standard,
    /// 1 for the primary descriptor, 2 for a supplementary one.
    pub kind: u8,
    /// The block it is recorded in, which High Sierra repeats inside it.
    pub location: u32,
    /// The volume identifier as recorded: d-characters, or UCS-2 under
    /// Joliet's escapes.
    pub volume_id: &'a [u8],
    /// The volume's size in blocks.
    pub volume_blocks: u32,
    /// The root directory's first block and length.
    pub root: (u32, u32),
    /// A supplementary descriptor's escape sequences (`b"%/E"`, Joliet level
    /// 3).
    pub escapes: Option<&'a [u8]>,
}

impl Descriptor<'_> {
    /// The descriptor's block.
    #[must_use]
    pub fn bytes(&self) -> Vec<u8> {
        let mut d = vec![0u8; BLOCK];
        let root = Rec {
            standard: self.standard,
            ..Rec::new(self.root.0, self.root.1, DIRECTORY, &[0])
        }
        .bytes();
        // Unused identifier positions are spaces: UCS-2 ones under Joliet.
        let pad = |field: &mut [u8], id: &[u8], wide: bool| {
            for (index, byte) in field.iter_mut().enumerate() {
                *byte = if !wide || index % 2 == 1 { b' ' } else { 0 };
            }
            field[..id.len()].copy_from_slice(id);
        };
        match self.standard {
            Standard::Iso9660 => {
                d[0] = self.kind;
                d[1..6].copy_from_slice(b"CD001");
                d[6] = 1;
                pad(&mut d[8..40], &[], false);
                pad(&mut d[40..72], self.volume_id, self.escapes.is_some());
                d[80..88].copy_from_slice(&both32(self.volume_blocks));
                if let Some(escapes) = self.escapes {
                    d[88..88 + escapes.len()].copy_from_slice(escapes);
                }
                d[120..124].copy_from_slice(&both16(1));
                d[124..128].copy_from_slice(&both16(1));
                d[128..132].copy_from_slice(&both16(BLOCK as u16));
                d[156..156 + root.len()].copy_from_slice(&root);
                d[881] = 1;
            }
            Standard::HighSierra => {
                d[0..8].copy_from_slice(&both32(self.location));
                d[8] = self.kind;
                d[9..14].copy_from_slice(b"CDROM");
                d[14] = 1;
                pad(&mut d[16..48], &[], false);
                pad(&mut d[48..80], self.volume_id, false);
                d[88..96].copy_from_slice(&both32(self.volume_blocks));
                d[128..132].copy_from_slice(&both16(1));
                d[132..136].copy_from_slice(&both16(1));
                d[136..140].copy_from_slice(&both16(BLOCK as u16));
                d[180..180 + root.len()].copy_from_slice(&root);
            }
        }
        d
    }
}

/// The set terminator of `standard`, recorded in block `location`.
#[must_use]
pub fn terminator(standard: Standard, location: u32) -> Vec<u8> {
    let mut d = vec![0u8; BLOCK];
    match standard {
        Standard::Iso9660 => {
            d[0] = 255;
            d[1..6].copy_from_slice(b"CD001");
            d[6] = 1;
        }
        Standard::HighSierra => {
            d[0..8].copy_from_slice(&both32(location));
            d[8] = 255;
            d[9..14].copy_from_slice(b"CDROM");
            d[14] = 1;
        }
    }
    d
}

/// SUSP and Rock Ridge entries.
pub mod rr {
    use super::{both32, vec, Vec};

    fn entry(signature: [u8; 2], body: &[u8]) -> Vec<u8> {
        let mut e = vec![signature[0], signature[1], (4 + body.len()) as u8, 1];
        e.extend_from_slice(body);
        e
    }

    /// The `SP` indicator skipping `skip` bytes and an `ER` naming RRIP
    /// 1.12, for the root's `.`.
    #[must_use]
    pub fn root(skip: u8) -> Vec<u8> {
        let mut out = vec![b'S', b'P', 7, 1, 0xBE, 0xEF, skip];
        let id = b"RRIP_1991A";
        let mut er = vec![id.len() as u8, 0, 0, 1];
        er.extend_from_slice(id);
        out.extend(entry(*b"ER", &er));
        out
    }

    /// A `PX`: the mode, link count, owner and group, and no serial.
    #[must_use]
    pub fn px(mode: u32, links: u32, uid: u32, gid: u32) -> Vec<u8> {
        let mut body = Vec::new();
        for value in [mode, links, uid, gid, 0] {
            body.extend_from_slice(&both32(value));
        }
        entry(*b"PX", &body)
    }

    /// An `NM` part, `continues` marking that another follows.
    #[must_use]
    pub fn nm(name: &[u8], continues: bool) -> Vec<u8> {
        let mut body = vec![u8::from(continues)];
        body.extend_from_slice(name);
        entry(*b"NM", &body)
    }

    /// An `SL` of component records, each `(flags, content)`.
    #[must_use]
    pub fn sl(components: &[(u8, &[u8])], continues: bool) -> Vec<u8> {
        let mut body = vec![u8::from(continues)];
        for (flags, content) in components {
            body.push(*flags);
            body.push(content.len() as u8);
            body.extend_from_slice(content);
        }
        entry(*b"SL", &body)
    }

    /// A `TF` of seven-byte stamps for the instants `flags` names.
    #[must_use]
    pub fn tf(flags: u8, stamps: &[[u8; 7]]) -> Vec<u8> {
        let mut body = vec![flags];
        for stamp in stamps {
            body.extend_from_slice(stamp);
        }
        entry(*b"TF", &body)
    }

    /// A `CE`: the rest of the entries are `len` bytes at `offset` in
    /// `block`.
    #[must_use]
    pub fn ce(block: u32, offset: u32, len: u32) -> Vec<u8> {
        let mut body = Vec::new();
        for value in [block, offset, len] {
            body.extend_from_slice(&both32(value));
        }
        entry(*b"CE", &body)
    }

    /// A `CL`: the directory relocated to `extent`.
    #[must_use]
    pub fn cl(extent: u32) -> Vec<u8> {
        entry(*b"CL", &both32(extent))
    }

    /// An `RE`: this directory was relocated here.
    #[must_use]
    pub fn re() -> Vec<u8> {
        entry(*b"RE", &[])
    }

    /// A sparse file's `SF`: its virtual `size`, high half then low, each
    /// both-endian, and a table depth of one.
    #[must_use]
    pub fn sf(size: u64) -> Vec<u8> {
        let mut body = both32((size >> 32) as u32).to_vec();
        body.extend_from_slice(&both32(size as u32));
        body.push(1);
        entry(*b"SF", &body)
    }

    /// A zisofs `ZF` of `2^log2_block`-byte blocks for a file `size` bytes
    /// long.
    #[must_use]
    pub fn zf(log2_block: u8, size: u32) -> Vec<u8> {
        let mut body = vec![b'p', b'z', 4, log2_block];
        body.extend_from_slice(&both32(size));
        entry(*b"ZF", &body)
    }

    /// An Amiga `AS`: the protection byte and comment text it carries, and
    /// whether the comment continues in the next.
    #[must_use]
    pub fn amiga(protection: Option<u8>, comment: Option<&[u8]>, continues: bool) -> Vec<u8> {
        let flags = u8::from(protection.is_some())
            | (u8::from(comment.is_some()) << 1)
            | (u8::from(continues) << 2);
        let mut body = vec![flags];
        if let Some(bits) = protection {
            body.extend_from_slice(&[0, 0, 0, bits]);
        }
        if let Some(text) = comment {
            body.push(text.len() as u8 + 1);
            body.extend_from_slice(text);
        }
        entry(*b"AS", &body)
    }
}

/// Apple's system use records (Technical Note FL36).
pub mod apple {
    use super::{vec, Vec};

    /// `AA` with HFS type, creator and Finder flags.
    #[must_use]
    pub fn hfs(file_type: [u8; 4], creator: [u8; 4], flags: u16) -> Vec<u8> {
        let mut out = vec![b'A', b'A', 14, 2];
        out.extend_from_slice(&file_type);
        out.extend_from_slice(&creator);
        out.extend_from_slice(&flags.to_be_bytes());
        out
    }

    /// `AA` with a `ProDOS` file type and auxiliary type.
    #[must_use]
    pub fn prodos(file_type: u8, aux: u16) -> Vec<u8> {
        let mut out = vec![b'A', b'A', 7, 1, file_type];
        out.extend_from_slice(&aux.to_le_bytes());
        out
    }

    /// The original, lengthless `BA` with HFS information.
    #[must_use]
    pub fn original_hfs(file_type: [u8; 4], creator: [u8; 4], flags: u16) -> Vec<u8> {
        let mut out = vec![b'B', b'A', 6];
        out.extend_from_slice(&file_type);
        out.extend_from_slice(&creator);
        out.extend_from_slice(&flags.to_be_bytes());
        out
    }

    /// `field` behind the fourteen bytes of a CD-ROM XA record.
    #[must_use]
    pub fn behind_xa(field: &[u8]) -> Vec<u8> {
        let mut out = vec![0, 0, 0, 0, 0x0D, 0x55, b'X', b'A', 1, 0, 0, 0, 0, 0];
        out.extend_from_slice(field);
        out
    }
}

/// The RISC OS system use field: `ARCHIMEDES`, the load and execution
/// addresses, and the attributes.
#[must_use]
pub fn archimedes(load: u32, exec: u32, attributes: u32) -> Vec<u8> {
    let mut field = vec![0u8; 32];
    field[..10].copy_from_slice(b"ARCHIMEDES");
    field[10..14].copy_from_slice(&load.to_le_bytes());
    field[14..18].copy_from_slice(&exec.to_le_bytes());
    field[18..22].copy_from_slice(&attributes.to_le_bytes());
    field
}

/// The load address of a file stamped with `filetype` and the start of
/// 1900 as its date.
#[must_use]
pub const fn typed_load(filetype: u16) -> u32 {
    0xFFF0_0000 | ((filetype as u32) << 8)
}

/// Where a directory's records landed.
struct Placed {
    extent: u32,
    len: u32,
}

/// Lays a tree out: directories first, depth first, then file data.
struct Layout {
    next: u32,
    image: Vec<u8>,
    directories: Vec<u32>,
    standard: Standard,
}

impl Layout {
    fn allocate(&mut self, blocks: usize) -> u32 {
        let at = self.next;
        self.next += blocks.max(1) as u32;
        let end = self.next as usize * BLOCK;
        if self.image.len() < end {
            self.image.resize(end, 0);
        }
        at
    }

    fn record<'a>(&self, extent: u32, len: u32, flags: u8, node: &'a Node) -> Rec<'a> {
        Rec {
            date: node.date,
            system_use: &node.system_use,
            standard: self.standard,
            ..Rec::new(extent, len, flags, &node.identifier)
        }
    }

    /// The records of `node`, a directory's child, its data placed.
    fn child_records(&mut self, node: &Node) -> Vec<Vec<u8>> {
        match &node.kind {
            Kind::Dir { .. } => {
                let placed = self.directory(node);
                vec![self
                    .record(placed.extent, placed.len, DIRECTORY | node.flags, node)
                    .bytes()]
            }
            Kind::Pointer { extent, len } => {
                vec![self.record(*extent, *len, node.flags, node).bytes()]
            }
            Kind::File {
                data,
                parts,
                xar,
                interleave,
            } => {
                let sizes = parts.clone().unwrap_or_else(|| vec![data.len()]);
                assert_eq!(
                    sizes.iter().sum::<usize>(),
                    data.len(),
                    "the parts are the file"
                );
                let mut out = Vec::new();
                let mut from = 0;
                for (index, size) in sizes.iter().enumerate() {
                    let bytes = &data[from..from + size];
                    from += size;
                    let last = index + 1 == sizes.len();
                    let flags = node.flags | if last { 0 } else { MULTI_EXTENT };
                    let xar_blocks = u8::from(xar.is_some());
                    let extent = match interleave {
                        None => {
                            let extent =
                                self.allocate(usize::from(xar_blocks) + size.div_ceil(BLOCK));
                            if let Some(xar) = xar {
                                self.write(extent as usize * BLOCK, xar);
                            }
                            self.write((extent as usize + usize::from(xar_blocks)) * BLOCK, bytes);
                            extent
                        }
                        Some((unit, gap)) => {
                            let unit_bytes = usize::from(*unit) * BLOCK;
                            let units = size.div_ceil(unit_bytes);
                            let stride = usize::from(*unit) + usize::from(*gap);
                            let lead = usize::from(xar_blocks);
                            let extent = self.allocate((units + lead) * stride);
                            if let Some(xar) = xar {
                                self.write(extent as usize * BLOCK, xar);
                            }
                            for (k, chunk) in bytes.chunks(unit_bytes).enumerate() {
                                let block = extent as usize + (k + lead) * stride;
                                self.write(block * BLOCK, chunk);
                            }
                            extent
                        }
                    };
                    out.push(
                        Rec {
                            interleave: interleave.unwrap_or((0, 0)),
                            xar_blocks,
                            ..self.record(extent, *size as u32, flags, node)
                        }
                        .bytes(),
                    );
                }
                out
            }
        }
    }

    fn write(&mut self, at: usize, bytes: &[u8]) {
        if self.image.len() < at + bytes.len() {
            self.image
                .resize((at + bytes.len()).div_ceil(BLOCK) * BLOCK, 0);
        }
        self.image[at..at + bytes.len()].copy_from_slice(bytes);
    }

    /// Lay `dir` out: its records packed so none crosses a sector, then its
    /// children's.
    fn directory(&mut self, dir: &Node) -> Placed {
        let Kind::Dir { children, dot_use } = &dir.kind else {
            unreachable!("a directory");
        };
        // The directory's own size depends only on its records' lengths, so
        // reserve it before placing the children.
        let lengths: Vec<usize> = core::iter::once(34 + dot_use.len())
            .chain(core::iter::once(34))
            .chain(children.iter().flat_map(|child| {
                let n = match &child.kind {
                    Kind::File { parts, .. } => parts.as_ref().map_or(1, Vec::len),
                    Kind::Dir { .. } | Kind::Pointer { .. } => 1,
                };
                let len = 33
                    + child.identifier.len()
                    + usize::from(child.identifier.len().is_multiple_of(2))
                    + child.system_use.len();
                core::iter::repeat_n(len, n)
            }))
            .collect();
        let mut pos = 0usize;
        for len in &lengths {
            if pos % BLOCK + len > BLOCK {
                pos = pos.next_multiple_of(BLOCK);
            }
            pos += len;
        }
        let sectors = pos.div_ceil(BLOCK);
        let extent = self.allocate(sectors);
        self.directories.push(extent);
        let len = (sectors * BLOCK) as u32;
        let dot = Rec {
            date: dir.date,
            system_use: dot_use,
            standard: self.standard,
            ..Rec::new(extent, len, DIRECTORY, &[0])
        };
        let dotdot = Rec {
            date: dir.date,
            standard: self.standard,
            ..Rec::new(extent, len, DIRECTORY, &[1])
        };
        let mut records = vec![dot.bytes(), dotdot.bytes()];
        for child in children {
            records.extend(self.child_records(child));
        }
        let mut at = extent as usize * BLOCK;
        let start = at;
        for r in records {
            if (at - start) % BLOCK + r.len() > BLOCK {
                at = start + (at - start).next_multiple_of(BLOCK);
            }
            self.write(at, &r);
            at += r.len();
        }
        Placed { extent, len }
    }
}

/// Master `disc` into an image: its descriptor set from block 16, then its
/// directories and its files' data from block 20.
#[must_use]
pub fn master(disc: &Disc) -> Mastered {
    let mut layout = Layout {
        next: 20,
        image: vec![0u8; 20 * BLOCK],
        directories: Vec::new(),
        standard: disc.standard,
    };
    let root = layout.directory(&disc.root);
    let joliet = match disc.standard {
        Standard::Iso9660 => disc.joliet.as_ref().map(|tree| layout.directory(tree)),
        Standard::HighSierra => None,
    };
    let volume_blocks = layout.next;
    let primary = Descriptor {
        standard: disc.standard,
        kind: 1,
        location: 16,
        volume_id: &disc.volume_id,
        volume_blocks,
        root: (root.extent, root.len),
        escapes: None,
    };
    layout.write(16 * BLOCK, &primary.bytes());
    let mut next = 17;
    if let Some(joliet) = joliet {
        let id = ucs2("JOLIET");
        let supplementary = Descriptor {
            kind: 2,
            location: next,
            volume_id: &id,
            root: (joliet.extent, joliet.len),
            escapes: Some(b"%/E"),
            ..primary
        };
        layout.write(next as usize * BLOCK, &supplementary.bytes());
        next += 1;
    }
    layout.write(next as usize * BLOCK, &terminator(disc.standard, next));
    Mastered {
        image: layout.image,
        directories: layout.directories,
    }
}

/// One file the RISC OS disc holds at its root.
pub struct RiscOsFile {
    /// Its identifier as recorded, without its version.
    pub recorded: &'static str,
    /// Its name as RISC OS reads the record back: a leading `_` its
    /// attributes mark as standing for `!`.
    pub name: &'static str,
    /// Its RISC OS filetype.
    pub filetype: u16,
    /// Its RISC OS attributes.
    pub attributes: u32,
    /// Its bytes.
    pub content: &'static [u8],
}

/// The volume identifier of the RISC OS disc, which its volume is named by.
pub const RISCOS_VOLUME: &str = "RISCOS";

/// The files of the RISC OS disc.
pub const RISCOS_FILES: [RiscOsFile; 2] = [
    RiscOsFile {
        recorded: "README",
        name: "README",
        filetype: 0xFFF,
        attributes: 0x03,
        content: b"A RISC OS text file, typed by its ARCHIMEDES record.\n",
    },
    RiscOsFile {
        recorded: "_BOOT",
        name: "!BOOT",
        filetype: 0xFEB,
        attributes: 0x103,
        content: b"| An Obey file\nEcho Booted\n",
    },
];

/// The volume identifier of the disc the optical vertical puts in the drive
/// in the RISC OS disc's place.
pub const SECOND_VOLUME: &str = "SECOND_DISC";

/// The files of the disc that replaces the RISC OS disc.
pub const SECOND_FILES: [RiscOsFile; 1] = [RiscOsFile {
    recorded: "SPRITES",
    name: "SPRITES",
    filetype: 0xFF9,
    attributes: 0x03,
    content: b"Not the first disc's bytes: a sprite file's.\n",
}];

/// The RISC OS disc the QEMU optical vertical copies from, as RISC OS
/// masters one: ISO 9660 names, and an `ARCHIMEDES` field on each of
/// [`RISCOS_FILES`] recording its filetype and attributes.
#[must_use]
pub fn riscos_disc() -> Vec<u8> {
    riscos_mastered(RISCOS_VOLUME, &RISCOS_FILES)
}

/// The disc the optical vertical changes the RISC OS disc for, mastered the
/// same way with [`SECOND_FILES`].
#[must_use]
pub fn second_disc() -> Vec<u8> {
    riscos_mastered(SECOND_VOLUME, &SECOND_FILES)
}

/// A disc of `files` at its root, each typed by its `ARCHIMEDES` field.
fn riscos_mastered(volume: &str, files: &[RiscOsFile]) -> Vec<u8> {
    let root = files
        .iter()
        .map(|file| {
            let identifier = alloc::format!("{}.;1", file.recorded);
            Node::file(identifier.as_bytes(), file.content).with_use(archimedes(
                typed_load(file.filetype),
                0,
                file.attributes,
            ))
        })
        .collect();
    master(&Disc {
        volume_id: volume.as_bytes().to_vec(),
        root: Node::dir(&[], root),
        ..Disc::default()
    })
    .image
}

#[cfg(test)]
mod tests;
