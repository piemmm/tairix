//! Extended attributes: the POSIX access ACL ext4 keeps in
//! `system.posix_acl_access`, and the inline data of a small file in
//! `system.data`.
//!
//! An attribute lives in the inode's tail after `i_extra_isize`, its values
//! measured from the first entry, or in the external block `i_file_acl`
//! names, measured from the block's start; a block may be shared between
//! inodes, its reference count saying by how many.
//!
//! On disk an ACL is ext4's own encoding: version 1, four-byte entries for
//! the owner, owning group, mask and other, eight-byte ones for named users
//! and groups.

use alloc::vec::Vec;

use tairix_abi::driver::block::Block;
use tairix_abi::driver::filesystem::{AclModel, NodeSecurity, SecurityAcl, SecuritySubject};
use tairix_abi::DriverError;

use crate::bytes::{
    align4, crc32c_zeroed, high32, le16, le32, low16, low32, put_le16, put_le32, scratch,
};
use crate::file::adjust_blocks;
use crate::inode::EXTRA_ISIZE;
use crate::superblock::Layout;
use crate::{Ext4, INODE_FILE_ACL_HI, INODE_FILE_ACL_LO};

const MAGIC: u32 = 0xEA02_0000;
const BLOCK_HEADER: usize = 32;
const IBODY_HEADER: usize = 4;
const ENTRY_HEADER: usize = 16;
const BLOCK_REFCOUNT: usize = 4;
const BLOCK_HASH: usize = 12;
const BLOCK_CHECKSUM: usize = 16;

/// `e_name_index` of `system.posix_acl_access`, whose name is empty.
const INDEX_POSIX_ACL_ACCESS: u8 = 2;
/// `e_name_index` of the `system.` namespace, holding `data`.
const INDEX_SYSTEM: u8 = 7;

const ACL_VERSION: u32 = 1;
const ACL_USER_OBJ: u16 = 0x01;
const ACL_USER: u16 = 0x02;
const ACL_GROUP_OBJ: u16 = 0x04;
const ACL_GROUP: u16 = 0x08;
const ACL_MASK: u16 = 0x10;
const ACL_OTHER: u16 = 0x20;

/// Where an attribute's value sits.
#[derive(Copy, Clone)]
struct Found {
    /// Byte offset of its entry header in the region.
    entry: usize,
    /// Byte range of its value in the region.
    value: (usize, usize),
}

/// Find attribute (`index`, `name`) among the entries from `start` in
/// `region`, values measured from `base`. `None` when absent; a malformed
/// region or a value kept in an EA inode is damage.
fn find(
    region: &[u8],
    start: usize,
    base: usize,
    index: u8,
    name: &[u8],
) -> Result<Option<Found>, DriverError> {
    let mut pos = start;
    loop {
        if pos + ENTRY_HEADER > region.len() || le32(region, pos) == 0 {
            return Ok(None);
        }
        let name_len = usize::from(region[pos]);
        let entry_index = region[pos + 1];
        let value_offs = usize::from(le16(region, pos + 2));
        let value_inum = le32(region, pos + 4);
        let value_size = le32(region, pos + 8) as usize;
        let entry_name = region
            .get(pos + ENTRY_HEADER..pos + ENTRY_HEADER + name_len)
            .ok_or(DriverError::DeviceFault)?;
        if entry_index == index && entry_name == name {
            if value_inum != 0 {
                return Err(DriverError::Unsupported);
            }
            let from = base
                .checked_add(value_offs)
                .ok_or(DriverError::DeviceFault)?;
            let to = from
                .checked_add(value_size)
                .ok_or(DriverError::DeviceFault)?;
            if to > region.len() {
                return Err(DriverError::DeviceFault);
            }
            return Ok(Some(Found {
                entry: pos,
                value: (from, to),
            }));
        }
        pos = pos
            .checked_add(align4(ENTRY_HEADER + name_len))
            .ok_or(DriverError::DeviceFault)?;
    }
}

/// The inode-body attribute region of record `raw`: where its entries
/// start, when it has one.
fn ibody(raw: &[u8]) -> Option<usize> {
    if raw.len() <= EXTRA_ISIZE {
        return None;
    }
    let header = EXTRA_ISIZE + usize::from(le16(raw, EXTRA_ISIZE));
    (header + IBODY_HEADER <= raw.len() && le32(raw, header) == MAGIC)
        .then_some(header + IBODY_HEADER)
}

/// An access ACL decoded from ext4's encoding.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct PosixAcl {
    user_obj: u8,
    group_obj: u8,
    mask: Option<u8>,
    other: u8,
    named: Vec<(SecuritySubject, u8)>,
}

impl PosixAcl {
    /// Decode `value`. Malformed — a wrong version, a torn entry, a missing
    /// or repeated required entry, or named entries with no mask — is
    /// refused rather than read as granting anything.
    fn decode(value: &[u8]) -> Result<Self, DriverError> {
        if value.len() < 4 || le32(value, 0) != ACL_VERSION {
            return Err(DriverError::DeviceFault);
        }
        let mut acl = Self::default();
        let mut seen = 0u16;
        let mut pos = 4;
        while pos < value.len() {
            let tag = le16(value, pos);
            let perm = (le16(value, pos + 2) & 0b111) as u8;
            match tag {
                ACL_USER | ACL_GROUP => {
                    if pos + 8 > value.len() {
                        return Err(DriverError::DeviceFault);
                    }
                    let id = le32(value, pos + 4);
                    let subject = if tag == ACL_USER {
                        SecuritySubject::User(id)
                    } else {
                        SecuritySubject::Group(id)
                    };
                    // A subject named twice has no single grant to apply.
                    if acl.named.iter().any(|&(named, _)| named == subject) {
                        return Err(DriverError::DeviceFault);
                    }
                    acl.named.try_reserve(1).map_err(|_| DriverError::NoSpace)?;
                    acl.named.push((subject, perm));
                    pos += 8;
                }
                ACL_USER_OBJ | ACL_GROUP_OBJ | ACL_MASK | ACL_OTHER => {
                    if pos + 4 > value.len() || seen & tag != 0 {
                        return Err(DriverError::DeviceFault);
                    }
                    seen |= tag;
                    match tag {
                        ACL_USER_OBJ => acl.user_obj = perm,
                        ACL_GROUP_OBJ => acl.group_obj = perm,
                        ACL_MASK => acl.mask = Some(perm),
                        _ => acl.other = perm,
                    }
                    pos += 4;
                }
                _ => return Err(DriverError::DeviceFault),
            }
        }
        let required = ACL_USER_OBJ | ACL_GROUP_OBJ | ACL_OTHER;
        if seen & required != required || (!acl.named.is_empty() && acl.mask.is_none()) {
            return Err(DriverError::DeviceFault);
        }
        Ok(acl)
    }

    /// Fold the ACL into `sec`. The entries the mode mirrors decide the
    /// mode, as they decide access on the system that wrote them; a minimal
    /// ACL is the mode bits alone.
    fn apply(&self, sec: &mut NodeSecurity) -> Result<(), DriverError> {
        let group_triad = self.mask.unwrap_or(self.group_obj);
        sec.mode = (sec.mode & 0o7000)
            | (u32::from(self.user_obj) << 6)
            | (u32::from(group_triad) << 3)
            | u32::from(self.other);
        let Some(mask) = self.mask else {
            return Ok(());
        };
        sec.acl_model = AclModel::Posix {
            group_obj: self.group_obj,
            mask,
        };
        for &(subject, perms) in &self.named {
            // More named entries than the record holds cannot be dropped:
            // a dropped one would fall through to a wider class.
            sec.push_acl(SecurityAcl { subject, perms })
                .map_err(|_| DriverError::Unsupported)?;
        }
        Ok(())
    }
}

/// Rewrite, in place, the entries of ACL `value` that mirror `mode`: the
/// owner, the mask (or the owning group, without one), and other — what a
/// `chmod` changes on a node with an ACL.
fn mirror_mode(value: &mut [u8], mode: u32) -> Result<(), DriverError> {
    let has_mask = PosixAcl::decode(value)?.mask.is_some();
    let triad = |shift: u32| ((mode >> shift) & 0b111) as u16;
    let mut pos = 4;
    while pos < value.len() {
        let tag = le16(value, pos);
        let set = match tag {
            ACL_USER_OBJ => Some(triad(6)),
            ACL_MASK => Some(triad(3)),
            ACL_GROUP_OBJ if !has_mask => Some(triad(3)),
            ACL_OTHER => Some(triad(0)),
            _ => None,
        };
        if let Some(perm) = set {
            put_le16(value, pos + 2, perm);
        }
        pos += if tag == ACL_USER || tag == ACL_GROUP {
            8
        } else {
            4
        };
    }
    Ok(())
}

/// The entry hash ext4 keeps for an attribute: its name, then its value's
/// little-endian words.
fn entry_hash(name: &[u8], value: &[u8]) -> u32 {
    let mut hash = 0u32;
    for &byte in name {
        hash = (hash << 5) ^ (hash >> 27) ^ u32::from(byte);
    }
    for word in value.chunks(4) {
        let mut padded = [0u8; 4];
        padded[..word.len()].copy_from_slice(word);
        hash = (hash << 16) ^ (hash >> 16) ^ u32::from_le_bytes(padded);
    }
    hash
}

/// The block hash over every entry's hash; zero when any entry's is.
fn block_hash(block: &[u8]) -> u32 {
    let mut hash = 0u32;
    let mut pos = BLOCK_HEADER;
    while pos + ENTRY_HEADER <= block.len() && le32(block, pos) != 0 {
        let e_hash = le32(block, pos + 12);
        if e_hash == 0 {
            return 0;
        }
        hash = (hash << 16) ^ (hash >> 16) ^ e_hash;
        pos += align4(ENTRY_HEADER + usize::from(block[pos]));
    }
    hash
}

/// The external attribute block record `raw` names, if any: the high half
/// is padding on a volume without `64bit`.
pub(crate) fn block_of(raw: &[u8], layout: &Layout) -> Option<u64> {
    let high = if layout.wide_desc() {
        u64::from(le16(raw, INODE_FILE_ACL_HI)) << 32
    } else {
        0
    };
    let block = u64::from(le32(raw, INODE_FILE_ACL_LO)) | high;
    (block != 0).then_some(block)
}

/// Restamp the hash of the entry at `entry` in `region`, whose value lies at
/// `value`; ext4 leaves an in-inode entry's hash zero, and so does this.
fn rehash_entry(region: &mut [u8], entry: usize, value: (usize, usize)) -> Result<(), DriverError> {
    let name_len = usize::from(*region.get(entry).ok_or(DriverError::DeviceFault)?);
    let name = region
        .get(entry + ENTRY_HEADER..entry + ENTRY_HEADER + name_len)
        .ok_or(DriverError::DeviceFault)?;
    let bytes = region
        .get(value.0..value.1)
        .ok_or(DriverError::DeviceFault)?;
    let hash = entry_hash(name, bytes);
    put_le32(region, entry + 12, hash);
    Ok(())
}

impl<B: Block> Ext4<B> {
    fn xattr_block_checksum(&self, block: u64, bytes: &[u8]) -> u32 {
        let c = tairix_crc32c::update(self.layout.csum_seed, &block.to_le_bytes());
        crc32c_zeroed::<4>(c, bytes, BLOCK_CHECKSUM)
    }

    /// Read attribute block `block`, its header and checksum checked.
    fn read_xattr_block(&mut self, block: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        let from_device = self.read_meta(block, buf)?;
        if le32(buf, 0) != MAGIC {
            return Err(DriverError::DeviceFault);
        }
        if from_device
            && self.layout.metadata_csum
            && le32(buf, BLOCK_CHECKSUM) != self.xattr_block_checksum(block, buf)
        {
            return Err(DriverError::DeviceFault);
        }
        Ok(())
    }

    /// Stage attribute block `block` with the entry `found` changed, its
    /// hashes and checksum restamped.
    fn put_xattr_block(
        &mut self,
        block: u64,
        buf: &mut [u8],
        found: Found,
    ) -> Result<(), DriverError> {
        rehash_entry(buf, found.entry, found.value)?;
        if le32(buf, BLOCK_HASH) != 0 {
            let rehash = block_hash(buf);
            put_le32(buf, BLOCK_HASH, rehash);
        }
        if self.layout.metadata_csum {
            let sum = self.xattr_block_checksum(block, buf);
            put_le32(buf, BLOCK_CHECKSUM, sum);
        }
        self.put_meta(block, buf)
    }

    /// Fold inode record `raw`'s access ACL into `sec`.
    pub(crate) fn decode_acl(
        &mut self,
        raw: &[u8],
        sec: &mut NodeSecurity,
    ) -> Result<(), DriverError> {
        if let Some(start) = ibody(raw) {
            if let Some(found) = find(raw, start, start, INDEX_POSIX_ACL_ACCESS, b"")? {
                return PosixAcl::decode(&raw[found.value.0..found.value.1])?.apply(sec);
            }
        }
        let Some(block) = block_of(raw, &self.layout) else {
            return Ok(());
        };
        let mut buf = scratch(self.layout.block_size as usize)?;
        self.read_xattr_block(block, &mut buf)?;
        if let Some(found) = find(&buf, BLOCK_HEADER, 0, INDEX_POSIX_ACL_ACCESS, b"")? {
            PosixAcl::decode(&buf[found.value.0..found.value.1])?.apply(sec)?;
        }
        Ok(())
    }

    /// Keep inode record `raw`'s access ACL in step with new mode bits
    /// `mode`, where it carries one. A shared attribute block is copied for
    /// this inode first, so no other inode's ACL changes.
    pub(crate) fn mirror_acl(&mut self, raw: &mut [u8], mode: u32) -> Result<(), DriverError> {
        if let Some(start) = ibody(raw) {
            if let Some(found) = find(raw, start, start, INDEX_POSIX_ACL_ACCESS, b"")? {
                mirror_mode(&mut raw[found.value.0..found.value.1], mode)?;
                if le32(raw, found.entry + 12) != 0 {
                    rehash_entry(raw, found.entry, found.value)?;
                }
                return Ok(());
            }
        }
        let Some(block) = block_of(raw, &self.layout) else {
            return Ok(());
        };
        let mut buf = scratch(self.layout.block_size as usize)?;
        self.read_xattr_block(block, &mut buf)?;
        let Some(found) = find(&buf, BLOCK_HEADER, 0, INDEX_POSIX_ACL_ACCESS, b"")? else {
            return Ok(());
        };
        let refs = le32(&buf, BLOCK_REFCOUNT);
        let target = if refs > 1 {
            let mut old = scratch(buf.len())?;
            old.copy_from_slice(&buf);
            put_le32(&mut old, BLOCK_REFCOUNT, refs - 1);
            if self.layout.metadata_csum {
                let sum = self.xattr_block_checksum(block, &old);
                put_le32(&mut old, BLOCK_CHECKSUM, sum);
            }
            self.put_meta(block, &old)?;
            // Every sharer already counts its attribute block, so trading
            // the shared one for a copy of its own leaves its count as it is.
            let (copy, _) = self.alloc_blocks(block, 1, u64::MAX)?;
            put_le32(&mut buf, BLOCK_REFCOUNT, 1);
            put_le32(raw, INODE_FILE_ACL_LO, low32(copy));
            put_le16(raw, INODE_FILE_ACL_HI, low16(high32(copy)));
            copy
        } else {
            block
        };
        mirror_mode(&mut buf[found.value.0..found.value.1], mode)?;
        self.put_xattr_block(target, &mut buf, found)
    }

    /// Give back the attribute block of an inode being deleted: freed once no
    /// other inode shares it.
    pub(crate) fn release_xattr_block(&mut self, raw: &mut [u8]) -> Result<(), DriverError> {
        let Some(block) = block_of(raw, &self.layout) else {
            return Ok(());
        };
        let mut buf = scratch(self.layout.block_size as usize)?;
        self.read_xattr_block(block, &mut buf)?;
        let refs = le32(&buf, BLOCK_REFCOUNT);
        if refs > 1 {
            put_le32(&mut buf, BLOCK_REFCOUNT, refs - 1);
            if self.layout.metadata_csum {
                let sum = self.xattr_block_checksum(block, &buf);
                put_le32(&mut buf, BLOCK_CHECKSUM, sum);
            }
            self.put_meta(block, &buf)?;
        } else {
            self.free_blocks(block, 1)?;
        }
        put_le32(raw, INODE_FILE_ACL_LO, 0);
        put_le16(raw, INODE_FILE_ACL_HI, 0);
        let cluster = i64::from(self.layout.cluster_size / self.layout.block_size);
        adjust_blocks(raw, &self.layout, -cluster)
    }
}

/// The inline data of record `raw` — the 60 bytes of `i_block`, then the
/// value of `system.data` — copied from byte `offset` into `buf`, up to the
/// file's `size`.
pub(crate) fn read_inline(
    raw: &[u8],
    size: u64,
    offset: u64,
    buf: &mut [u8],
) -> Result<usize, DriverError> {
    const IBLOCK: usize = crate::I_BLOCK_OFFSET;
    const INLINE: usize = 60;
    let mut content = Vec::new();
    content
        .try_reserve(INLINE)
        .map_err(|_| DriverError::NoSpace)?;
    content.extend_from_slice(&raw[IBLOCK..IBLOCK + INLINE]);
    if let Some(start) = ibody(raw) {
        if let Some(found) = find(raw, start, start, INDEX_SYSTEM, b"data")? {
            let value = &raw[found.value.0..found.value.1];
            content
                .try_reserve(value.len())
                .map_err(|_| DriverError::NoSpace)?;
            content.extend_from_slice(value);
        }
    }
    if size > content.len() as u64 {
        return Err(DriverError::DeviceFault);
    }
    if offset >= size {
        return Ok(0);
    }
    let start = usize::try_from(offset).map_err(|_| DriverError::DeviceFault)?;
    let end = usize::try_from(size)
        .map_err(|_| DriverError::DeviceFault)?
        .min(start + buf.len());
    buf[..end - start].copy_from_slice(&content[start..end]);
    Ok(end - start)
}

#[cfg(test)]
#[path = "xattr_tests.rs"]
mod tests;
