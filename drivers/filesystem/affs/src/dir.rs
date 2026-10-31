//! Directories: the hash table each keeps, the chains it files entries in,
//! and the changes that file, unfile and move them.

use alloc::vec::Vec;

use tairix_abi::driver::block::Block;
use tairix_abi::driver::filesystem::{DirEntry, DirVisit, NodeId};
use tairix_abi::DriverError;
use tairix_fsprobe::affs::NAME_MAX;

use crate::guard::Guard;
use crate::header::{low32, Header, Kind};
use crate::name::Name;
use crate::Affs;

/// One entry of a bucket a listing has yet to hand over: where it is filed,
/// and its name.
struct Listed {
    block: u64,
    name: Name,
}

impl<B: Block> Affs<B> {
    /// The entry named `name` in `dir`: its own header, a hard link's and
    /// not its target's.
    pub(crate) fn find(
        &mut self,
        dir: &Header,
        name: &Name,
    ) -> Result<Option<Header>, DriverError> {
        let bucket = name.bucket(self.matching, self.at.table_len());
        let mut next = dir.table(bucket);
        let mut guard = Guard::new(dir.block, self.disk.blocks);
        while next != 0 {
            guard.step(u64::from(next))?;
            let entry = self.filed(dir, next)?;
            if entry.name()?.matches(name, self.matching) {
                return Ok(Some(entry));
            }
            next = entry.hash_chain();
        }
        Ok(None)
    }

    /// The header filed at `block` in `dir`'s table: one `dir` keeps as its
    /// own, so the volume stays a tree.
    fn filed(&mut self, dir: &Header, block: u32) -> Result<Header, DriverError> {
        let entry = self.header(u64::from(block))?;
        if entry.kind == Kind::Root || u64::from(entry.parent()) != dir.block {
            return Err(DriverError::BadMagic);
        }
        Ok(entry)
    }

    /// Hand `visit` the entries of `dir` from `cursor` on.
    ///
    /// The order is the table's buckets, and within one the entries' header
    /// blocks ascending, whatever order its chain holds them in, so an entry
    /// keeps its place however others are filed around it. A cursor is the
    /// bucket and the last block handed over from it. What an entry names is
    /// read only as it is handed over.
    pub(crate) fn list(
        &mut self,
        dir: &Header,
        cursor: u64,
        visit: &mut dyn FnMut(&DirEntry, &[u8]) -> DirVisit,
    ) -> Result<(), DriverError> {
        let table_len = self.at.table_len();
        let mut bucket = usize::try_from(cursor >> 32).unwrap_or(usize::MAX);
        let mut after = cursor & u64::from(u32::MAX);
        let mut listed: Vec<Listed> = Vec::new();
        let mut text = [0u8; 2 * NAME_MAX];
        while bucket < table_len {
            listed.clear();
            let mut next = dir.table(bucket);
            let mut guard = Guard::new(dir.block, self.disk.blocks);
            while next != 0 {
                guard.step(u64::from(next))?;
                let entry = self.filed(dir, next)?;
                next = entry.hash_chain();
                if entry.block <= after {
                    continue;
                }
                let name = entry.name()?;
                if !tairix_util::fallible::push(
                    &mut listed,
                    Listed {
                        block: entry.block,
                        name,
                    },
                ) {
                    return Err(DriverError::OutOfMemory);
                }
            }
            listed.sort_unstable_by_key(|entry| entry.block);
            for entry in &listed {
                let filed = self.filed(dir, low32(entry.block))?;
                let node = self.entry_node(filed)?;
                let handed = DirEntry {
                    node: NodeId::from_raw(node.block),
                    info: self.info(&node)?,
                    next_cursor: (bucket as u64) << 32 | entry.block,
                };
                let len = entry.name.decode(&mut text);
                if visit(&handed, &text[..len]) == DirVisit::Stop {
                    return Ok(());
                }
            }
            bucket += 1;
            after = 0;
        }
        Ok(())
    }

    /// File `entry` in `dir`'s chain for its name, the chain kept in
    /// ascending block order. `entry` is written before anything points at
    /// it; `dir` is left for the caller to write when its own slot changed.
    pub(crate) fn file_entry(
        &mut self,
        dir: &mut Header,
        entry: &mut Header,
    ) -> Result<(), DriverError> {
        let bucket = entry.name()?.bucket(self.matching, self.at.table_len());
        let head = dir.table(bucket);
        if head == 0 || u64::from(head) > entry.block {
            entry.set_hash_chain(head);
            self.write_header(entry)?;
            dir.set_table(bucket, low32(entry.block));
            return Ok(());
        }
        let mut guard = Guard::new(dir.block, self.disk.blocks);
        let mut prev = self.filed(dir, head)?;
        loop {
            let next = prev.hash_chain();
            if next == 0 || u64::from(next) > entry.block {
                break;
            }
            guard.step(u64::from(next))?;
            prev = self.filed(dir, next)?;
        }
        entry.set_hash_chain(prev.hash_chain());
        self.write_header(entry)?;
        prev.set_hash_chain(low32(entry.block));
        self.write_header(&mut prev)
    }

    /// Take `entry` out of `dir`'s chain for its name; `dir` is left for the
    /// caller to write.
    pub(crate) fn unfile_entry(
        &mut self,
        dir: &mut Header,
        entry: &Header,
    ) -> Result<(), DriverError> {
        let bucket = entry.name()?.bucket(self.matching, self.at.table_len());
        let head = dir.table(bucket);
        if u64::from(head) == entry.block {
            dir.set_table(bucket, entry.hash_chain());
            return Ok(());
        }
        let mut guard = Guard::new(dir.block, self.disk.blocks);
        let mut at = head;
        while at != 0 {
            guard.step(u64::from(at))?;
            let mut prev = self.filed(dir, at)?;
            if u64::from(prev.hash_chain()) == entry.block {
                prev.set_hash_chain(entry.hash_chain());
                return self.write_header(&mut prev);
            }
            at = prev.hash_chain();
        }
        Err(DriverError::BadMagic)
    }

    /// Write `dir` as a change to its entries leaves it: dated now.
    fn touch(&mut self, dir: &mut Header) -> Result<(), DriverError> {
        dir.set_date(self.now());
        self.write_header(dir)
    }

    /// Whether `dir` files no entry.
    pub(crate) fn is_empty(dir: &Header) -> bool {
        (0..dir.table_len()).all(|slot| dir.table(slot) == 0)
    }

    /// Create an entry of `kind` named `name` in `dir`, a soft link storing
    /// `target`.
    pub(crate) fn create_entry(
        &mut self,
        dir: NodeId,
        name: &[u8],
        kind: Kind,
        target: Option<&[u8]>,
    ) -> Result<NodeId, DriverError> {
        let name = Name::encode(name)?;
        self.change(|fs| {
            let mut dir = fs.directory(dir)?;
            if fs.find(&dir, &name)?.is_some() {
                return Err(DriverError::AlreadyExists);
            }
            let block = fs.allocate(None)?;
            let buf = fs.disk.buffer()?;
            let mut entry = Header::new(block, kind, buf, fs.at, &name, dir.block, fs.now());
            if let Some(target) = target {
                entry.set_link_target(target);
            }
            fs.file_entry(&mut dir, &mut entry)?;
            fs.touch(&mut dir)?;
            Ok(NodeId::from_raw(block))
        })
    }

    /// Unlink `name` from `dir`.
    pub(crate) fn remove_entry(&mut self, dir: NodeId, name: &[u8]) -> Result<(), DriverError> {
        let Ok(name) = Name::encode(name) else {
            return Err(DriverError::NotFound);
        };
        self.change(|fs| {
            let mut dir = fs.directory(dir)?;
            let entry = fs.find(&dir, &name)?.ok_or(DriverError::NotFound)?;
            if entry.kind == Kind::Directory && !Self::is_empty(&entry) {
                return Err(DriverError::DirectoryNotEmpty);
            }
            if entry.kind == Kind::Directory && entry.next_link() != 0 {
                fs.check_promotion(&entry)?;
            }
            fs.drop_name(&mut dir, entry)
        })
    }

    /// Take the name `entry` gives an object out of `dir`: a link's own, an
    /// object's promoting its newest link to take its place, or the object's
    /// last, freeing it once no hold remains. Every refusal is decided
    /// before the first write.
    fn drop_name(&mut self, dir: &mut Header, entry: Header) -> Result<(), DriverError> {
        match entry.kind {
            Kind::FileLink | Kind::DirLink => return self.remove_link(dir, &entry),
            _ if entry.next_link() != 0 => return self.promote(dir, entry),
            _ => {}
        }
        self.unfile_entry(dir, &entry)?;
        self.touch(dir)?;
        self.unname(entry.block)
    }

    /// Unfile the hard link `link` from `dir`, take it out of its object's
    /// chain of links, and free it — once its last unpin comes, for a link to
    /// a directory the kernel holds.
    fn remove_link(&mut self, dir: &mut Header, link: &Header) -> Result<(), DriverError> {
        let real = self.linked(link)?.block;
        self.unfile_entry(dir, link)?;
        self.touch(dir)?;
        // Unfiling may have rewritten the object's own chain pointer, and a
        // directory link may be filed in the directory it names.
        let real = self.header(real)?;
        self.unchain_link(real, link)?;
        self.unname(link.block)
    }

    /// The object hard link `link` names, which must name it back.
    fn linked(&mut self, link: &Header) -> Result<Header, DriverError> {
        let real = self.header(u64::from(link.real_entry()))?;
        let kind = match link.kind {
            Kind::FileLink => Kind::File,
            _ => Kind::Directory,
        };
        if real.kind != kind {
            return Err(DriverError::BadMagic);
        }
        Ok(real)
    }

    /// Take `link` out of the chain of links `real` heads.
    fn unchain_link(&mut self, mut real: Header, link: &Header) -> Result<(), DriverError> {
        if u64::from(real.next_link()) == link.block {
            real.set_next_link(link.next_link());
            return self.write_header(&mut real);
        }
        let mut guard = Guard::new(real.block, self.disk.blocks);
        let mut at = real.next_link();
        while at != 0 {
            guard.step(u64::from(at))?;
            let mut prev = self.header(u64::from(at))?;
            if u64::from(prev.real_entry()) != real.block {
                return Err(DriverError::BadMagic);
            }
            if u64::from(prev.next_link()) == link.block {
                prev.set_next_link(link.next_link());
                return self.write_header(&mut prev);
            }
            at = prev.next_link();
        }
        Err(DriverError::BadMagic)
    }

    /// Refuse promoting a directory's newest link when the link lies within
    /// the directory itself: the directory would be filed inside its own
    /// subtree.
    fn check_promotion(&mut self, real: &Header) -> Result<(), DriverError> {
        let link = self.header(u64::from(real.next_link()))?;
        if self.within(u64::from(link.parent()), real.block)? {
            return Err(DriverError::DirectoryCycle);
        }
        Ok(())
    }

    /// Remove the object `real`'s name from `dir` while hard links to it
    /// remain: as AmigaDOS does, its newest link's name and place pass to the
    /// object, which keeps its block, its data and every other link.
    fn promote(&mut self, dir: &mut Header, mut real: Header) -> Result<(), DriverError> {
        let link = self.header(u64::from(real.next_link()))?;
        if self.linked(&link)?.block != real.block {
            return Err(DriverError::BadMagic);
        }
        self.unfile_entry(dir, &real)?;
        self.touch(dir)?;
        // Unfiling the object may have rewritten the link's own chain pointer.
        let link = self.header(link.block)?;
        let mut place = self.header(u64::from(link.parent()))?;
        if !place.kind.is_directory() {
            return Err(DriverError::BadMagic);
        }
        self.unfile_entry(&mut place, &link)?;
        real.set_name(&link.name()?);
        real.set_parent(place.block);
        real.set_next_link(link.next_link());
        self.file_entry(&mut place, &mut real)?;
        self.touch(&mut place)?;
        self.unname(link.block)
    }

    /// Whether directory `dir` lies within `ancestor`'s subtree, `ancestor`
    /// itself included.
    pub(crate) fn within(&mut self, dir: u64, ancestor: u64) -> Result<bool, DriverError> {
        let mut at = dir;
        let mut guard = Guard::new(dir, self.disk.blocks);
        loop {
            if at == ancestor {
                return Ok(true);
            }
            let header = self.header(at)?;
            match header.kind {
                Kind::Root => return Ok(false),
                Kind::Directory => {}
                _ => return Err(DriverError::BadMagic),
            }
            at = u64::from(header.parent());
            guard.step(at)?;
        }
    }

    /// Move `src_name` of `src_dir` to `dst_name` of `dst_dir`, replacing an
    /// entry of a kind it may replace.
    pub(crate) fn rename_entry(
        &mut self,
        src_dir: NodeId,
        src_name: &[u8],
        dst_dir: NodeId,
        dst_name: &[u8],
    ) -> Result<(), DriverError> {
        let Ok(src_name) = Name::encode(src_name) else {
            return Err(DriverError::NotFound);
        };
        let dst_name = Name::encode(dst_name)?;
        let matching = self.matching;
        self.change(|fs| {
            let mut src = fs.directory(src_dir)?;
            let mut entry = fs.find(&src, &src_name)?.ok_or(DriverError::NotFound)?;
            let same_dir = src_dir == dst_dir;
            if same_dir && src_name.matches(&dst_name, matching) {
                // The same entry: a change of spelling at most, filed in the
                // bucket the fold already chose.
                if entry.name()? != dst_name {
                    entry.set_name(&dst_name);
                    fs.write_header(&mut entry)?;
                }
                return Ok(());
            }
            let dst = if same_dir {
                src.block
            } else {
                fs.directory(dst_dir)?.block
            };
            let dst_header = fs.header(dst)?;
            let occupant = fs.find(&dst_header, &dst_name)?;
            if let Some(occupant) = &occupant {
                let moved = fs.header(entry.block)?;
                let moved = fs.entry_node(moved)?.block;
                let replaced = fs.header(occupant.block)?;
                if fs.entry_node(replaced)?.block == moved {
                    // Two names of one file: POSIX does nothing.
                    return Ok(());
                }
                match (entry.kind.is_directory(), occupant.kind.is_directory()) {
                    (true, true) if !Self::is_empty(occupant) => {
                        return Err(DriverError::DirectoryNotEmpty)
                    }
                    (true, true) | (false, false) => {}
                    _ => return Err(DriverError::Unsupported),
                }
                if occupant.kind == Kind::Directory && occupant.next_link() != 0 {
                    fs.check_promotion(occupant)?;
                }
            }
            if entry.kind == Kind::Directory && !same_dir && fs.within(dst, entry.block)? {
                return Err(DriverError::DirectoryCycle);
            }
            if let Some(occupant) = occupant {
                let mut place = fs.header(dst)?;
                fs.drop_name(&mut place, occupant)?;
                // Dropping the occupant may have rewritten the moved entry's
                // chain pointer, and the directory both share.
                src = fs.header(src.block)?;
                entry = fs.header(entry.block)?;
            }
            fs.unfile_entry(&mut src, &entry)?;
            fs.touch(&mut src)?;
            let mut dst = fs.header(dst)?;
            entry.set_name(&dst_name);
            entry.set_parent(dst.block);
            fs.file_entry(&mut dst, &mut entry)?;
            fs.touch(&mut dst)
        })
    }

    /// Add `name` in `dir` as a hard link to the file `node`.
    pub(crate) fn link_entry(
        &mut self,
        dir: NodeId,
        name: &[u8],
        node: NodeId,
    ) -> Result<(), DriverError> {
        let name = Name::encode(name)?;
        self.change(|fs| {
            let mut dir = fs.directory(dir)?;
            let real = fs.node(node)?;
            if real.kind != Kind::File {
                return Err(DriverError::Unsupported);
            }
            if fs.unnamed(real.block) {
                return Err(DriverError::NotFound);
            }
            if fs.find(&dir, &name)?.is_some() {
                return Err(DriverError::AlreadyExists);
            }
            let block = fs.allocate(None)?;
            let buf = fs.disk.buffer()?;
            let mut link = Header::new(
                block,
                Kind::FileLink,
                buf,
                fs.at,
                &name,
                dir.block,
                fs.now(),
            );
            link.set_real_entry(real.block);
            link.set_next_link(real.next_link());
            fs.file_entry(&mut dir, &mut link)?;
            fs.touch(&mut dir)?;
            let mut real = fs.header(real.block)?;
            real.set_next_link(low32(block));
            fs.write_header(&mut real)
        })
    }

    /// Free the object whose header is `block`, its last name gone and no
    /// hold left: a file's data and extension blocks, then the header.
    pub(crate) fn free_object(&mut self, block: u64) -> Result<(), DriverError> {
        let header = self.header(block)?;
        if header.kind == Kind::File {
            self.free_data(&header)?;
        }
        self.free_header(block)
    }

    /// Wipe and free a header block, so an id that named it names nothing.
    pub(crate) fn free_header(&mut self, block: u64) -> Result<(), DriverError> {
        let zeros = self.disk.buffer()?;
        self.put(block, &zeros)?;
        self.release(block)
    }
}
