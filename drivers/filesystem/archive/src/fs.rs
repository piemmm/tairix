//! The served archive (`plans/ARCHIVES.md` §3): the filesystem driver traits
//! over the member tree.
//!
//! Reads go to where a member's bytes are. A change edits the tree, and new
//! bytes wait in the staging file; the change is committed — the next archive
//! written from the tree and made the archive — when the commit window
//! closes, on `flush`, and before the volume leaves. A member removed while
//! held keeps its bytes past a commit: they are moved to the staging file
//! first, since the archive they were read from is gone afterwards.

use alloc::collections::BTreeMap;

use tairix_abi::blkio::BlkCommandCost;
use tairix_abi::driver::filesystem::{
    AllocationClass, DirEntry, DirVisit, FilesystemAttrs, FilesystemAttrsFs,
    FilesystemAttrsProvider, FilesystemRead, FilesystemSecurity, FilesystemStats, FilesystemWrite,
    NameMatching, NodeId, NodeIdentity, NodeInfo, NodeKind, NodeSecurity, NodeTimes, Unpinned,
    VolumeHost, VolumeStats, WriteRefusal,
};
use tairix_abi::driver::DriverHandle;
use tairix_abi::time::Time64;
use tairix_abi::volume::{ARCHIVE_STAGING, ARCHIVE_WORK_FILES};
use tairix_abi::DriverError;
use tairix_fsmeta::preset::acorn;
use tairix_fsserve::VolumeAccess;
use tairix_writeback::CommitScheduler;

use crate::data::{Check, Coding, Cursors, Data, Lead, Staged};
use crate::format::Format;
use crate::region::Regions;
use crate::riscos;
use crate::tree::{Body, Meta, Node, Tree, Unheld, ROOT};

/// The block size a served archive reports.
const BLOCK: u32 = 512;

/// The served archive.
pub struct ArchiveFs<A: VolumeAccess> {
    regions: Regions<A>,
    format: Format,
    tree: Tree,
    cursors: Cursors,
    /// Where the next staged byte goes in the staging file.
    staged_end: u64,
    /// Whether the tree says something the archive does not.
    dirty: bool,
    refusal: Option<WriteRefusal>,
    /// Why a commit failed: the archive is written no more, and every flush
    /// answers it.
    failed: Option<DriverError>,
    schedule: CommitScheduler,
}

impl<A: VolumeAccess> ArchiveFs<A> {
    /// The archive `regions` holds, read as `format` reads it into `tree`,
    /// its members read through `stream` where it is one compressed stream
    /// or `rar` where it is a RAR archive, written only where `refusal` is
    /// `None`.
    pub(crate) fn new(
        regions: Regions<A>,
        format: Format,
        tree: Tree,
        (stream, rar): (Option<crate::stream::Stream>, Option<crate::rar::Reader>),
        refusal: Option<WriteRefusal>,
    ) -> Self {
        let mut cursors = Cursors::default();
        cursors.clear(stream);
        cursors.set_rar(rar);
        Self {
            regions,
            format,
            tree,
            cursors,
            staged_end: 0,
            dirty: false,
            refusal,
            failed: None,
            // An archive's commit rewrites it whole, the dearest command a
            // volume has.
            schedule: CommitScheduler::new(BlkCommandCost::Bridged),
        }
    }

    /// What the mount table calls the archive.
    pub(crate) const fn format_name(&self) -> tairix_abi::volume::FormatName {
        self.format.name()
    }

    /// Why the archive is not written, if it is not.
    pub(crate) const fn write_refusal(&self) -> Option<WriteRefusal> {
        self.refusal
    }

    /// Refuse a change while the archive is not written.
    fn writable(&self) -> Result<(), DriverError> {
        match self.refusal {
            None => Ok(()),
            Some(_) => Err(DriverError::ReadOnly),
        }
    }

    /// The instant a change is stamped with, or the epoch where the host
    /// knows none.
    fn now(&self) -> Time64 {
        self.schedule
            .host()
            .and_then(|(_, host)| host.wall_time())
            .unwrap_or(Time64::UNIX_EPOCH)
    }

    /// Run the change `op`, then commit where the window has closed: once
    /// `op` is done the change stands, and a commit failing after it is
    /// answered by the next flush.
    fn change<T>(
        &mut self,
        op: impl FnOnce(&mut Self) -> Result<T, DriverError>,
    ) -> Result<T, DriverError> {
        self.writable()?;
        let done = op(self)?;
        self.dirty = true;
        self.schedule.opened();
        self.schedule.joined();
        if !self.schedule.defers() || self.schedule.expired() {
            self.commit();
        }
        Ok(done)
    }

    /// Write the next archive from the tree and make it the archive; one
    /// failing stops the archive being written.
    fn commit(&mut self) {
        if self.failed.is_some() || !self.dirty {
            return;
        }
        if let Err(err) = self.write_next() {
            self.failed = Some(err);
            self.refusal = Some(WriteRefusal::CommitFailed);
        }
    }

    fn write_next(&mut self) -> Result<(), DriverError> {
        self.keep_held_orphans()?;
        let written = self
            .format
            .write(&self.tree, &self.regions, &mut self.cursors)?;
        self.regions.commit(written.len)?;
        // The archive the cursors read is gone.
        self.cursors.clear(written.stream);
        for placed in written.placed {
            let node = self.tree.node_mut(placed.id)?;
            if let (Some(data), Body::File(body)) = (placed.data, &mut node.body) {
                *body = data;
            }
            node.kept = placed.kept;
            if let Some(notes) = placed.notes {
                node.notes = notes;
            }
        }
        let staged = self.tree.nodes().any(
            |(_, node)| matches!(&node.body, Body::File(Data::Staged(staged)) if staged.len > 0),
        );
        // Staging that cannot be cut is carried on after, its room spent.
        if !staged && self.regions.truncate(ARCHIVE_STAGING, 0).is_ok() {
            self.staged_end = 0;
        }
        self.dirty = false;
        self.schedule.closed();
        Ok(())
    }

    /// Move to the staging file the bytes of every held node no name
    /// reaches that are still in the archive, which the commit replaces.
    /// Bytes this driver cannot read are never moved: a hold reads them as
    /// it did, refused, and the commit goes on.
    fn keep_held_orphans(&mut self) -> Result<(), DriverError> {
        let orphans: alloc::vec::Vec<u64> = self
            .tree
            .nodes()
            .filter(|(_, node)| {
                !node.named
                    && matches!(
                        &node.body,
                        Body::File(Data::Archived(archived))
                            if !matches!(archived.coding, Coding::Refused { .. })
                    )
            })
            .map(|(id, _)| id)
            .collect();
        for id in orphans {
            match self.stage(id) {
                Ok(_) => {}
                // Damaged where they were read: no move can keep them, and a
                // hold's reads of them fail as this one did.
                Err(DriverError::MediumError) => self.unreadable(id)?,
                Err(err) => return Err(err),
            }
        }
        Ok(())
    }

    /// Make `id`'s bytes unreadable, every read of them refused as damaged.
    fn unreadable(&mut self, id: u64) -> Result<(), DriverError> {
        let node = self.tree.node_mut(id)?;
        let len = match &node.body {
            Body::File(data) => data.len(),
            _ => return Err(DriverError::Unsupported),
        };
        node.body = Body::File(Data::Archived(crate::data::Archived {
            at: 0,
            lead: Lead::None,
            len,
            coding: Coding::Refused {
                why: DriverError::MediumError,
                packed: 0,
            },
            check: Check::None,
        }));
        self.cursors.forget(id);
        Ok(())
    }

    /// The node `id`'s bytes, moved to the staging file where they are in
    /// the archive: what a change to them starts from.
    fn stage(&mut self, id: u64) -> Result<&mut Staged, DriverError> {
        let node = self.tree.node(id)?;
        let (Body::File(data), gen) = (&node.body, node.content_gen) else {
            return Err(DriverError::Unsupported);
        };
        if let Data::Archived(archived) = data {
            let archived = *archived;
            let archived = crate::zip::located(&self.regions, archived)?;
            let mut staged = Staged::default();
            let at = self.staged_end;
            match archived.coding {
                // Bytes held to a check pass through it, so a changed copy
                // never takes a fresh check over damaged bytes.
                Coding::Stored if archived.check == Check::None => {
                    self.regions.copy(
                        (tairix_abi::volume::ARCHIVE_REGION, archived.at),
                        (ARCHIVE_STAGING, at),
                        archived.len,
                    )?;
                }
                Coding::Refused { why, .. } => return Err(why),
                Coding::Stored
                | Coding::Deflate { .. }
                | Coding::Bzip2 { .. }
                | Coding::Arc { .. }
                | Coding::Stream
                | Coding::Rar { .. } => {
                    let mut buf = tairix_util::fallible::filled(64 * 1024, 0u8)
                        .ok_or(DriverError::OutOfMemory)?;
                    let mut offset = 0;
                    while offset < archived.len {
                        let got = self.cursors.read(
                            &self.regions,
                            (id, gen),
                            &archived,
                            offset,
                            &mut buf,
                        )?;
                        if got == 0 {
                            return Err(DriverError::MediumError);
                        }
                        self.regions
                            .write(ARCHIVE_STAGING, at + offset, &buf[..got])?;
                        offset += got as u64;
                    }
                }
            }
            staged.wrote(0, archived.len, at)?;
            self.staged_end = at + archived.len;
            self.cursors.forget(id);
            let node = self.tree.node_mut(id)?;
            node.body = Body::File(Data::Staged(staged));
        }
        match &mut self.tree.node_mut(id)?.body {
            Body::File(Data::Staged(staged)) => Ok(staged),
            _ => Err(DriverError::Unsupported),
        }
    }

    /// The structural record of `id`.
    fn info(&self, id: u64) -> Result<NodeInfo, DriverError> {
        let node = self.tree.node(id)?;
        Ok(info_of(id, node))
    }

    fn file_node(&self, id: u64) -> Result<&Node, DriverError> {
        let node = self.tree.node(id)?;
        match node.body {
            Body::File(_) => Ok(node),
            _ => Err(DriverError::Unsupported),
        }
    }
}

/// What `node`, numbered `id`, reports of itself.
fn info_of(id: u64, node: &Node) -> NodeInfo {
    let (size, allocated) = match &node.body {
        Body::Dir(_) => (0, 0),
        Body::File(Data::Archived(archived)) => (archived.len, archived.packed()),
        Body::File(Data::Staged(staged)) => (staged.len, staged.len),
        Body::Link(target) => (target.len() as u64, 0),
    };
    // An archive records no name counts.
    let nlink = if node.named { NodeInfo::SINGLE_NAME } else { 0 };
    NodeInfo {
        kind: node.kind(),
        nlink,
        ino: id,
        size,
        allocated,
        times: node.meta.times,
        content_gen: node.content_gen,
    }
}

impl<A: VolumeAccess> FilesystemRead for ArchiveFs<A> {
    fn root(&self) -> NodeId {
        NodeId::from_raw(ROOT)
    }

    fn name_matching(&self) -> NameMatching {
        self.tree.matching()
    }

    fn node_identity(&self) -> NodeIdentity {
        NodeIdentity::Stable
    }

    fn node_info(&mut self, node: NodeId) -> Result<NodeInfo, DriverError> {
        self.info(node.raw())
    }

    fn lookup(&mut self, dir: NodeId, name: &[u8]) -> Result<NodeId, DriverError> {
        self.tree.lookup(dir.raw(), name).map(NodeId::from_raw)
    }

    fn parent(&mut self, dir: NodeId) -> Result<NodeId, DriverError> {
        self.tree.entries(dir.raw())?;
        Ok(NodeId::from_raw(self.tree.node(dir.raw())?.parent))
    }

    fn read_at(&mut self, file: NodeId, offset: u64, buf: &mut [u8]) -> Result<usize, DriverError> {
        let id = file.raw();
        let node = self.file_node(id)?;
        let gen = node.content_gen;
        let Body::File(data) = &node.body else {
            return Err(DriverError::Unsupported);
        };
        match data {
            Data::Staged(staged) => staged.read(&self.regions, offset, buf),
            Data::Archived(archived) => {
                let mut archived = *archived;
                if archived.lead != Lead::None {
                    // Placed once, the first time its bytes are read.
                    archived = crate::zip::located(&self.regions, archived)?;
                    if let Body::File(Data::Archived(kept)) = &mut self.tree.node_mut(id)?.body {
                        *kept = archived;
                    }
                }
                self.cursors
                    .read(&self.regions, (id, gen), &archived, offset, buf)
            }
        }
    }

    fn read_link(&mut self, link: NodeId, out: &mut [u8]) -> Result<usize, DriverError> {
        let Body::Link(target) = &self.tree.node(link.raw())?.body else {
            return Err(DriverError::Unsupported);
        };
        let out = out
            .get_mut(..target.len())
            .ok_or(DriverError::BufferTooSmall)?;
        out.copy_from_slice(target);
        Ok(target.len())
    }

    fn read_dir(
        &mut self,
        dir: NodeId,
        cursor: u64,
        after: &[u8],
        visit: &mut dyn FnMut(&DirEntry, &[u8]) -> DirVisit,
    ) -> Result<(), DriverError> {
        let entries = self.tree.entries(dir.raw())?;
        // A listing resumes after the name its cursor came from, wherever
        // entries made or removed meanwhile have moved it.
        let from = if cursor == 0 {
            core::ops::Bound::Unbounded
        } else {
            let mut key = alloc::vec::Vec::new();
            key.try_reserve_exact(after.len())
                .map_err(|_| DriverError::OutOfMemory)?;
            match self.tree.matching() {
                NameMatching::AsciiCaseInsensitive => {
                    key.extend(after.iter().map(u8::to_ascii_lowercase));
                }
                _ => key.extend_from_slice(after),
            }
            core::ops::Bound::Excluded(key)
        };
        for (position, (_, &id)) in entries
            .range((from, core::ops::Bound::Unbounded))
            .enumerate()
        {
            let node = self.tree.node(id)?;
            let entry = DirEntry {
                node: NodeId::from_raw(id),
                info: info_of(id, node),
                next_cursor: cursor + position as u64 + 1,
            };
            if visit(&entry, &node.name) == DirVisit::Stop {
                break;
            }
        }
        Ok(())
    }

    fn pin(&mut self, node: NodeId) -> Result<NodeId, DriverError> {
        self.tree.pin(node.raw())?;
        Ok(node)
    }

    fn unpin(&mut self, pinned: NodeId) -> Result<Unpinned, DriverError> {
        let unheld = self.tree.unpin(pinned.raw())?;
        if unheld == Unheld::Freed {
            self.cursors.forget(pinned.raw());
        }
        Ok(match unheld {
            Unheld::Held => Unpinned::Held,
            Unheld::Released => Unpinned::Released,
            Unheld::Freed => Unpinned::Freed,
        })
    }

    fn release_pins(&mut self) -> Result<(), DriverError> {
        self.tree.release_pins();
        Ok(())
    }
}

impl<A: VolumeAccess> ArchiveFs<A> {
    /// The record a node of `kind` made through a create is born with: as
    /// much of `record` as the format keeps, and the unrecorded stand-in for
    /// the rest.
    fn born(&self, kind: NodeKind, record: &NodeSecurity) -> Meta {
        let now = self.now();
        let mode = if self.format.keeps_modes() {
            record.mode
        } else {
            NodeSecurity::unrecorded(kind).mode
        };
        Meta {
            mode: mode & 0o7777,
            owner: self
                .format
                .keeps_owners()
                .then_some((record.uid, record.gid)),
            times: NodeTimes {
                created: now,
                modified: now,
                accessed: now,
                changed: now,
            },
            acorn: None,
        }
    }

    /// Refuse a name the format cannot spell.
    fn spellable(&self, name: &[u8]) -> Result<(), DriverError> {
        if !crate::name::servable(name) {
            return Err(DriverError::LengthOutOfRange);
        }
        self.format.admits_name(name)
    }

    /// Refuse a change that writes the records of `id` and what it holds
    /// afresh where one of them is a member whose bytes this driver cannot
    /// read: its record would have to be written whole, and it cannot be.
    fn rewritable(&self, id: u64) -> Result<(), DriverError> {
        let mut pending = alloc::vec::Vec::new();
        pending
            .try_reserve(1)
            .map_err(|_| DriverError::OutOfMemory)?;
        pending.push(id);
        while let Some(at) = pending.pop() {
            match &self.tree.node(at)?.body {
                Body::File(Data::Archived(archived)) => {
                    if let Coding::Refused { why, .. } = archived.coding {
                        return Err(why);
                    }
                }
                Body::Dir(entries) => {
                    pending
                        .try_reserve(entries.len())
                        .map_err(|_| DriverError::OutOfMemory)?;
                    pending.extend(entries.values().copied());
                }
                Body::File(Data::Staged(_)) | Body::Link(_) => {}
            }
        }
        Ok(())
    }

    /// Refuse a file reaching past what the format can record of one.
    fn recordable(&self, end: u64) -> Result<(), DriverError> {
        if end <= self.format.largest_member() {
            Ok(())
        } else {
            Err(DriverError::LengthOutOfRange)
        }
    }

    /// The instant a change to `id`'s bytes is stamped with now, and the
    /// RISC OS fields that stamp leaves it: a typed object's stamp is its one
    /// time.
    fn stamp(&self, id: u64) -> Result<(Time64, Option<acorn::Object>), DriverError> {
        let now = self.now();
        let acorn = self
            .tree
            .node(id)?
            .meta
            .acorn
            .map(|object| riscos::dated(object, now))
            .transpose()?;
        Ok((now, acorn))
    }

    /// Note that `id`'s bytes changed, at `stamp`: a fresh generation.
    fn touched(
        &mut self,
        id: u64,
        (now, acorn): (Time64, Option<acorn::Object>),
    ) -> Result<(), DriverError> {
        let gen = self.tree.fresh_gen();
        let node = self.tree.node_mut(id)?;
        node.content_gen = gen;
        node.meta.times.modified = now;
        node.meta.times.changed = now;
        node.meta.acorn = acorn;
        node.kept = None;
        self.cursors.forget(id);
        Ok(())
    }

    /// `after`, a change of `id`'s RISC OS fields from `before`, and the
    /// modification time it leaves: a stamp set dates the member, and an
    /// object the change types is dated as the member is.
    fn dating(
        &self,
        id: u64,
        before: acorn::Object,
        after: acorn::Object,
    ) -> Result<(acorn::Object, Time64), DriverError> {
        let modified = self.tree.node(id)?.meta.times.modified;
        Ok(match (riscos::date_of(&before), riscos::date_of(&after)) {
            (None, Some(_)) => (riscos::dated(after, modified)?, modified),
            (Some(old), Some(new)) if old != new => (after, new),
            _ => (after, modified),
        })
    }
}

impl<A: VolumeAccess> FilesystemWrite for ArchiveFs<A> {
    fn create(
        &mut self,
        dir: NodeId,
        name: &[u8],
        kind: NodeKind,
        record: &NodeSecurity,
    ) -> Result<NodeId, DriverError> {
        supported(self.format.holds_tree())?;
        self.spellable(name)?;
        let meta = self.born(kind, record);
        let body = match kind {
            NodeKind::Directory => Body::Dir(BTreeMap::new()),
            NodeKind::RegularFile => Body::File(Data::empty()),
            NodeKind::Symlink => return Err(DriverError::Unsupported),
        };
        self.change(|fs| fs.tree.add(dir.raw(), name, body, meta, None))
            .map(NodeId::from_raw)
    }

    fn create_link(
        &mut self,
        dir: NodeId,
        name: &[u8],
        target: &[u8],
        record: &NodeSecurity,
    ) -> Result<NodeId, DriverError> {
        supported(self.format.keeps_links())?;
        self.spellable(name)?;
        let meta = self.born(NodeKind::Symlink, record);
        let mut owned = alloc::vec::Vec::new();
        owned
            .try_reserve_exact(target.len())
            .map_err(|_| DriverError::OutOfMemory)?;
        owned.extend_from_slice(target);
        self.change(|fs| fs.tree.add(dir.raw(), name, Body::Link(owned), meta, None))
            .map(NodeId::from_raw)
    }

    fn write_at(&mut self, file: NodeId, offset: u64, data: &[u8]) -> Result<usize, DriverError> {
        let id = file.raw();
        self.file_node(id)?;
        let end = offset
            .checked_add(data.len() as u64)
            .ok_or(DriverError::LengthOutOfRange)?;
        self.recordable(end)?;
        self.change(|fs| {
            let stamp = fs.stamp(id)?;
            fs.stage(id)?;
            let at = fs.staged_end;
            fs.regions.write(ARCHIVE_STAGING, at, data)?;
            fs.staged_end += data.len() as u64;
            fs.stage(id)?.wrote(offset, data.len() as u64, at)?;
            fs.touched(id, stamp)?;
            Ok(data.len())
        })
    }

    fn truncate(&mut self, file: NodeId, size: u64) -> Result<(), DriverError> {
        let id = file.raw();
        self.file_node(id)?;
        self.recordable(size)?;
        self.change(|fs| {
            let stamp = fs.stamp(id)?;
            let node = fs.tree.node_mut(id)?;
            match &mut node.body {
                // Cutting to nothing, or a stored member held to no check to a
                // shorter length, needs no bytes moved.
                Body::File(Data::Archived(_)) if size == 0 => node.body = Body::File(Data::empty()),
                Body::File(Data::Archived(archived))
                    if matches!(archived.coding, Coding::Stored | Coding::Stream)
                        && archived.check == Check::None
                        && size <= archived.len =>
                {
                    archived.len = size;
                }
                _ => fs.stage(id)?.truncate(size),
            }
            fs.touched(id, stamp)
        })
    }

    fn remove(&mut self, dir: NodeId, name: &[u8]) -> Result<(), DriverError> {
        supported(self.format.holds_tree())?;
        self.change(|fs| {
            let id = fs.tree.unlink(dir.raw(), name)?;
            if fs.tree.node(id).is_err() {
                fs.cursors.forget(id);
            }
            Ok(())
        })
    }

    fn rename(
        &mut self,
        src_dir: NodeId,
        src_name: &[u8],
        dst_dir: NodeId,
        dst_name: &[u8],
    ) -> Result<(), DriverError> {
        self.spellable(dst_name)?;
        let moving = self.tree.lookup(src_dir.raw(), src_name)?;
        self.rewritable(moving)?;
        self.change(|fs| {
            if let Some(replaced) =
                fs.tree
                    .rename(src_dir.raw(), src_name, dst_dir.raw(), dst_name)?
            {
                if fs.tree.node(replaced).is_err() {
                    fs.cursors.forget(replaced);
                }
            }
            Ok(())
        })
    }

    fn flush(&mut self) -> Result<(), DriverError> {
        self.commit();
        if let Some(failed) = self.failed {
            return Err(failed);
        }
        self.regions.flush()
    }

    fn set_volume_host(&mut self, volume: DriverHandle, host: &'static dyn VolumeHost) {
        self.schedule.set_host(volume, host);
    }

    fn set_allocation_class(&mut self, _class: AllocationClass) {}
}

/// A member reports the owner its archive recorded, else the stand-in an
/// ownerless format reports, with the mode the archive recorded or was born
/// with.
impl<A: VolumeAccess> FilesystemSecurity for ArchiveFs<A> {
    fn security(&mut self, node: NodeId) -> Result<NodeSecurity, DriverError> {
        let node = self.tree.node(node.raw())?;
        let (uid, gid) = node.meta.owner.unwrap_or((0, 0));
        Ok(NodeSecurity::new(node.meta.mode, uid, gid))
    }

    /// Store `security`'s mode, and its owner where the format keeps one. No
    /// format has a place for an ACL or a capability gate, so a record the
    /// format cannot hold whole is refused rather than kept in part.
    fn set_security(&mut self, node: NodeId, security: NodeSecurity) -> Result<(), DriverError> {
        let id = node.raw();
        if id == ROOT {
            supported(self.format.keeps_root())?;
        }
        self.rewritable(id)?;
        let current = self.security(node)?;
        let owner_moves = (security.uid, security.gid) != (current.uid, current.gid);
        supported(
            self.format.keeps_modes()
                && security.required_cap.is_none()
                && security.acl().is_empty()
                && (self.format.keeps_owners() || !owner_moves),
        )?;
        let keeps_owners = self.format.keeps_owners();
        self.change(|fs| {
            if id == ROOT {
                fs.format.record_root();
            }
            let node = fs.tree.node_mut(id)?;
            node.meta.mode = security.mode & 0o7777;
            if keeps_owners {
                node.meta.owner = Some((security.uid, security.gid));
            }
            node.kept = None;
            Ok(())
        })
    }

    fn records_ownership(&mut self, node: NodeId) -> Result<bool, DriverError> {
        Ok(self.tree.node(node.raw())?.meta.owner.is_some())
    }
}

/// Refuse an operation the archive's format has no place for.
fn supported(kept: bool) -> Result<(), DriverError> {
    if kept {
        Ok(())
    } else {
        Err(DriverError::Unsupported)
    }
}

impl<A: VolumeAccess> FilesystemStats for ArchiveFs<A> {
    fn stats(&mut self) -> Result<VolumeStats, DriverError> {
        // What the archive could grow into is its own volume's, which a
        // driver of the archive cannot see: none is reported free.
        Ok(VolumeStats {
            block_size: BLOCK,
            in_reserve: false,
            total_blocks: self.regions.len().div_ceil(u64::from(BLOCK)),
            free_blocks: 0,
            avail_blocks: 0,
            files: self.tree.len() as u64,
            files_free: 0,
        })
    }
}

impl<A: VolumeAccess> FilesystemAttrs for ArchiveFs<A> {
    fn get_attr(
        &mut self,
        node: NodeId,
        key: &[u8],
        value_out: &mut [u8],
    ) -> Result<Option<usize>, DriverError> {
        let key = tairix_fsmeta::AttrKey::parse(key).map_err(|_| DriverError::OutOfRange)?;
        let Some(object) = self.tree.node(node.raw())?.meta.acorn else {
            return Ok(None);
        };
        let mut staging = [0u8; acorn::ATTR_VALUE_MAX];
        let Some(len) = object
            .value(key.as_bytes(), &mut staging)
            .map_err(|_| DriverError::OutOfRange)?
        else {
            return Ok(None);
        };
        let out = value_out
            .get_mut(..len)
            .ok_or(DriverError::BufferTooSmall)?;
        out.copy_from_slice(&staging[..len]);
        Ok(Some(len))
    }

    fn set_attr(&mut self, node: NodeId, key: &[u8], value: &[u8]) -> Result<(), DriverError> {
        let key = tairix_fsmeta::AttrKey::parse(key).map_err(|_| DriverError::OutOfRange)?;
        let id = node.raw();
        if id == ROOT {
            return Err(DriverError::Unsupported);
        }
        supported(self.format.keeps_attributes())?;
        self.rewritable(id)?;
        let current = self.tree.node(id)?;
        let directory = matches!(current.body, Body::Dir(_));
        let before = current.meta.acorn.unwrap_or_else(|| untyped(directory));
        let mut object = before;
        object.set(key.as_bytes(), value, self.format.acorn_attr_bits())?;
        let (object, modified) = self.dating(id, before, object)?;
        self.change(|fs| {
            let node = fs.tree.node_mut(id)?;
            node.meta.acorn = Some(object);
            node.meta.times.modified = modified;
            node.kept = None;
            Ok(())
        })
    }

    fn list_attr(
        &mut self,
        node: NodeId,
        index: u64,
        key_out: &mut [u8],
    ) -> Result<Option<usize>, DriverError> {
        let Some(object) = self.tree.node(node.raw())?.meta.acorn else {
            return Ok(None);
        };
        let Some(key) = object.key(index) else {
            return Ok(None);
        };
        let out = key_out
            .get_mut(..key.len())
            .ok_or(DriverError::BufferTooSmall)?;
        out.copy_from_slice(key);
        Ok(Some(key.len()))
    }

    fn remove_attr(&mut self, node: NodeId, key: &[u8]) -> Result<(), DriverError> {
        supported(self.format.keeps_attributes())?;
        let key = tairix_fsmeta::AttrKey::parse(key).map_err(|_| DriverError::OutOfRange)?;
        let id = node.raw();
        self.rewritable(id)?;
        let Some(before) = self.tree.node(id)?.meta.acorn else {
            return Err(DriverError::NotFound);
        };
        let mut object = before;
        object.remove(key.as_bytes())?;
        let (object, modified) = self.dating(id, before, object)?;
        self.change(|fs| {
            let node = fs.tree.node_mut(id)?;
            node.meta.acorn = Some(object);
            node.meta.times.modified = modified;
            node.kept = None;
            Ok(())
        })
    }
}

/// The RISC OS fields a member without any is given when one is set: an
/// untyped object, readable and writable by its owner.
fn untyped(directory: bool) -> acorn::Object {
    let dir = if directory { acorn::ATTR_DIRECTORY } else { 0 };
    acorn::Object {
        load: 0,
        exec: 0,
        attr: acorn::attr_from_fileswitch(0x03) | dir,
    }
}

impl<A: VolumeAccess> FilesystemAttrsProvider for ArchiveFs<A> {
    fn attrs_fs(&mut self) -> Option<&mut dyn FilesystemAttrsFs> {
        Some(self)
    }
}

/// The work files a writable archive attachment has; an attachment with
/// fewer is read only.
pub(crate) const fn writable_attachment(work_files: u8) -> bool {
    work_files >= ARCHIVE_WORK_FILES
}

#[cfg(test)]
#[path = "fs_tests.rs"]
mod tests;
