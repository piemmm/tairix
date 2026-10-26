//! A small in-memory volume the server is tested against: a tree of
//! directories, files and links, numbered nodes, counted holds, attributes,
//! and a record of the allocation class each call ran under.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use tairix_abi::driver::filesystem::{
    AllocationClass, DirEntry, DirVisit, FilesystemAttrs, FilesystemAttrsFs,
    FilesystemAttrsProvider, FilesystemRead, FilesystemSecurity, FilesystemStats, FilesystemWrite,
    NameMatching, NodeId, NodeIdentity, NodeInfo, NodeKind, NodeSecurity, NodeTimes, Unpinned,
    VolumeStats,
};
use tairix_abi::DriverError;

enum Body {
    Dir(BTreeMap<Vec<u8>, u64>),
    File(Vec<u8>),
    Link(Vec<u8>),
}

struct Node {
    body: Body,
    security: NodeSecurity,
    /// The directory holding its name; `None` once no name reaches it.
    parent: Option<u64>,
}

/// The test volume.
pub(crate) struct TestFs {
    nodes: BTreeMap<u64, Node>,
    attrs: BTreeMap<(u64, Vec<u8>), Vec<u8>>,
    /// How many holds each held node has.
    holds: BTreeMap<u64, u32>,
    next: u64,
    /// Whether [`FilesystemAttrsProvider::attrs_fs`] answers.
    pub(crate) keeps_attrs: bool,
    /// The class each mutating call ran under, in order.
    pub(crate) classes: Vec<AllocationClass>,
    class: AllocationClass,
    /// Bytes a read reports beyond what it wrote: a driver that lies.
    pub(crate) overcount: usize,
    /// How many flushes the volume saw.
    pub(crate) flushes: usize,
}

pub(crate) const ROOT: NodeId = NodeId::from_raw(1);

impl TestFs {
    pub(crate) fn new() -> Self {
        let mut nodes = BTreeMap::new();
        nodes.insert(
            ROOT.raw(),
            Node {
                body: Body::Dir(BTreeMap::new()),
                security: NodeSecurity::unrecorded(NodeKind::Directory),
                parent: Some(ROOT.raw()),
            },
        );
        Self {
            nodes,
            attrs: BTreeMap::new(),
            holds: BTreeMap::new(),
            next: 2,
            keeps_attrs: true,
            classes: Vec::new(),
            class: AllocationClass::Ordinary,
            overcount: 0,
            flushes: 0,
        }
    }

    fn node(&self, node: NodeId) -> Result<&Node, DriverError> {
        self.nodes.get(&node.raw()).ok_or(DriverError::NotFound)
    }

    /// A directory that lost its name takes no entries.
    fn entries(&self, dir: NodeId) -> Result<&BTreeMap<Vec<u8>, u64>, DriverError> {
        let node = self.node(dir)?;
        match &node.body {
            Body::Dir(_) if node.parent.is_none() => Err(DriverError::NotFound),
            Body::Dir(entries) => Ok(entries),
            _ => Err(DriverError::Unsupported),
        }
    }

    fn entries_mut(&mut self, dir: NodeId) -> Result<&mut BTreeMap<Vec<u8>, u64>, DriverError> {
        self.entries(dir)?;
        match self.nodes.get_mut(&dir.raw()).map(|n| &mut n.body) {
            Some(Body::Dir(entries)) => Ok(entries),
            _ => Err(DriverError::Unsupported),
        }
    }

    fn child(&self, dir: NodeId, name: &[u8]) -> Result<u64, DriverError> {
        self.entries(dir)?
            .get(name)
            .copied()
            .ok_or(DriverError::NotFound)
    }

    fn file_mut(&mut self, file: NodeId) -> Result<&mut Vec<u8>, DriverError> {
        match self.nodes.get_mut(&file.raw()).map(|n| &mut n.body) {
            Some(Body::File(data)) => Ok(data),
            Some(_) => Err(DriverError::Unsupported),
            None => Err(DriverError::NotFound),
        }
    }

    /// Take `id`'s name away, freeing it unless a hold keeps it.
    fn unname(&mut self, id: u64) {
        if self.holds.contains_key(&id) {
            if let Some(node) = self.nodes.get_mut(&id) {
                node.parent = None;
            }
        } else {
            self.free(id);
        }
    }

    fn free(&mut self, id: u64) {
        self.nodes.remove(&id);
        self.attrs.retain(|(node, _), _| *node != id);
    }

    fn add(
        &mut self,
        dir: NodeId,
        name: &[u8],
        body: Body,
        record: &NodeSecurity,
    ) -> Result<NodeId, DriverError> {
        self.classes.push(self.class);
        if self.entries(dir)?.contains_key(name) {
            return Err(DriverError::AlreadyExists);
        }
        let id = self.next;
        self.next += 1;
        self.nodes.insert(
            id,
            Node {
                body,
                security: *record,
                parent: Some(dir.raw()),
            },
        );
        self.entries_mut(dir)?.insert(name.to_vec(), id);
        Ok(NodeId::from_raw(id))
    }

    fn info_of(id: u64, node: &Node) -> NodeInfo {
        let (kind, size) = match &node.body {
            Body::Dir(_) => (NodeKind::Directory, 0),
            Body::File(data) => (NodeKind::RegularFile, data.len() as u64),
            Body::Link(target) => (NodeKind::Symlink, target.len() as u64),
        };
        NodeInfo {
            kind,
            nlink: u32::from(node.parent.is_some()),
            ino: id,
            size,
            allocated: size,
            times: NodeTimes::default(),
            content_gen: NodeInfo::NO_CONTENT_GEN,
        }
    }
}

impl FilesystemRead for TestFs {
    fn root(&self) -> NodeId {
        ROOT
    }

    fn name_matching(&self) -> NameMatching {
        NameMatching::Exact
    }

    fn node_identity(&self) -> NodeIdentity {
        NodeIdentity::Stable
    }

    fn node_info(&mut self, node: NodeId) -> Result<NodeInfo, DriverError> {
        Ok(Self::info_of(node.raw(), self.node(node)?))
    }

    fn lookup(&mut self, dir: NodeId, name: &[u8]) -> Result<NodeId, DriverError> {
        self.child(dir, name).map(NodeId::from_raw)
    }

    fn parent(&mut self, dir: NodeId) -> Result<NodeId, DriverError> {
        self.entries(dir)?;
        self.node(dir)?
            .parent
            .map(NodeId::from_raw)
            .ok_or(DriverError::NotFound)
    }

    fn read_at(&mut self, file: NodeId, offset: u64, buf: &mut [u8]) -> Result<usize, DriverError> {
        let Body::File(data) = &self.node(file)?.body else {
            return Err(DriverError::Unsupported);
        };
        let start = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(data.len());
        let len = (data.len() - start).min(buf.len());
        buf[..len].copy_from_slice(&data[start..start + len]);
        Ok(len + self.overcount)
    }

    fn read_link(&mut self, link: NodeId, out: &mut [u8]) -> Result<usize, DriverError> {
        let Body::Link(target) = &self.node(link)?.body else {
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
        let entries = self.entries(dir)?;
        let skip = if cursor == 0 {
            0
        } else {
            entries.range(..=after.to_vec()).count()
        };
        for (position, (name, id)) in entries.iter().enumerate().skip(skip) {
            let entry = DirEntry {
                node: NodeId::from_raw(*id),
                info: Self::info_of(*id, &self.nodes[id]),
                next_cursor: position as u64 + 1,
            };
            if visit(&entry, name) == DirVisit::Stop {
                break;
            }
        }
        Ok(())
    }

    fn pin(&mut self, node: NodeId) -> Result<NodeId, DriverError> {
        self.node(node)?;
        *self.holds.entry(node.raw()).or_insert(0) += 1;
        Ok(node)
    }

    fn unpin(&mut self, pinned: NodeId) -> Result<Unpinned, DriverError> {
        let held = self
            .holds
            .get_mut(&pinned.raw())
            .ok_or(DriverError::NotFound)?;
        *held -= 1;
        if *held > 0 {
            return Ok(Unpinned::Held);
        }
        self.holds.remove(&pinned.raw());
        if self.node(pinned)?.parent.is_some() {
            return Ok(Unpinned::Released);
        }
        self.free(pinned.raw());
        Ok(Unpinned::Freed)
    }

    fn release_pins(&mut self) -> Result<(), DriverError> {
        for (id, _) in core::mem::take(&mut self.holds) {
            if self
                .nodes
                .get(&id)
                .is_some_and(|node| node.parent.is_none())
            {
                self.free(id);
            }
        }
        Ok(())
    }
}

impl FilesystemWrite for TestFs {
    fn create(
        &mut self,
        dir: NodeId,
        name: &[u8],
        kind: NodeKind,
        record: &NodeSecurity,
    ) -> Result<NodeId, DriverError> {
        let body = match kind {
            NodeKind::Directory => Body::Dir(BTreeMap::new()),
            NodeKind::RegularFile => Body::File(Vec::new()),
            NodeKind::Symlink => return Err(DriverError::Unsupported),
        };
        self.add(dir, name, body, record)
    }

    fn create_link(
        &mut self,
        dir: NodeId,
        name: &[u8],
        target: &[u8],
        record: &NodeSecurity,
    ) -> Result<NodeId, DriverError> {
        self.add(dir, name, Body::Link(target.to_vec()), record)
    }

    fn write_at(&mut self, file: NodeId, offset: u64, data: &[u8]) -> Result<usize, DriverError> {
        self.classes.push(self.class);
        let file = self.file_mut(file)?;
        let start = usize::try_from(offset).map_err(|_| DriverError::NoSpace)?;
        let end = start + data.len();
        if file.len() < end {
            file.resize(end, 0);
        }
        file[start..end].copy_from_slice(data);
        Ok(data.len())
    }

    fn truncate(&mut self, file: NodeId, size: u64) -> Result<(), DriverError> {
        let size = usize::try_from(size).map_err(|_| DriverError::NoSpace)?;
        self.file_mut(file)?.resize(size, 0);
        Ok(())
    }

    fn punch(&mut self, file: NodeId, offset: u64, len: u64) -> Result<(), DriverError> {
        let bytes = self.file_mut(file)?;
        let at = |bound: u64| usize::try_from(bound).map_or(bytes.len(), |b| b.min(bytes.len()));
        let (start, end) = (at(offset), at(offset.saturating_add(len)));
        bytes[start..end].fill(0);
        Ok(())
    }

    fn remove(&mut self, dir: NodeId, name: &[u8]) -> Result<(), DriverError> {
        let id = self.child(dir, name)?;
        if matches!(&self.nodes[&id].body, Body::Dir(e) if !e.is_empty()) {
            return Err(DriverError::DirectoryNotEmpty);
        }
        self.entries_mut(dir)?.remove(name);
        self.unname(id);
        Ok(())
    }

    fn rename(
        &mut self,
        src_dir: NodeId,
        src_name: &[u8],
        dst_dir: NodeId,
        dst_name: &[u8],
    ) -> Result<(), DriverError> {
        let id = self.child(src_dir, src_name)?;
        self.entries(dst_dir)?;
        self.entries_mut(src_dir)?.remove(src_name);
        if let Some(replaced) = self.entries_mut(dst_dir)?.insert(dst_name.to_vec(), id) {
            self.unname(replaced);
        }
        if let Some(node) = self.nodes.get_mut(&id) {
            node.parent = Some(dst_dir.raw());
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), DriverError> {
        self.flushes += 1;
        Ok(())
    }

    fn set_allocation_class(&mut self, class: AllocationClass) {
        self.class = class;
    }
}

impl FilesystemSecurity for TestFs {
    fn security(&mut self, node: NodeId) -> Result<NodeSecurity, DriverError> {
        Ok(self.node(node)?.security)
    }

    fn set_security(&mut self, node: NodeId, security: NodeSecurity) -> Result<(), DriverError> {
        self.nodes
            .get_mut(&node.raw())
            .ok_or(DriverError::NotFound)?
            .security = security;
        Ok(())
    }
}

impl FilesystemStats for TestFs {
    fn stats(&mut self) -> Result<VolumeStats, DriverError> {
        Ok(VolumeStats {
            block_size: 512,
            in_reserve: false,
            total_blocks: 100,
            free_blocks: 90,
            avail_blocks: 90,
            files: 10,
            files_free: 10 - self.nodes.len().min(10) as u64,
        })
    }
}

impl FilesystemAttrs for TestFs {
    fn get_attr(
        &mut self,
        node: NodeId,
        key: &[u8],
        value_out: &mut [u8],
    ) -> Result<Option<usize>, DriverError> {
        self.node(node)?;
        let Some(value) = self.attrs.get(&(node.raw(), key.to_vec())) else {
            return Ok(None);
        };
        let out = value_out
            .get_mut(..value.len())
            .ok_or(DriverError::BufferTooSmall)?;
        out.copy_from_slice(value);
        Ok(Some(value.len()))
    }

    fn set_attr(&mut self, node: NodeId, key: &[u8], value: &[u8]) -> Result<(), DriverError> {
        self.node(node)?;
        self.attrs
            .insert((node.raw(), key.to_vec()), value.to_vec());
        Ok(())
    }

    fn list_attr(
        &mut self,
        node: NodeId,
        index: u64,
        key_out: &mut [u8],
    ) -> Result<Option<usize>, DriverError> {
        let Some(((_, key), _)) = self
            .attrs
            .range((node.raw(), Vec::new())..)
            .take_while(|((n, _), _)| *n == node.raw())
            .nth(usize::try_from(index).unwrap_or(usize::MAX))
        else {
            return Ok(None);
        };
        let out = key_out
            .get_mut(..key.len())
            .ok_or(DriverError::BufferTooSmall)?;
        out.copy_from_slice(key);
        Ok(Some(key.len()))
    }

    fn remove_attr(&mut self, node: NodeId, key: &[u8]) -> Result<(), DriverError> {
        self.attrs
            .remove(&(node.raw(), key.to_vec()))
            .map(|_| ())
            .ok_or(DriverError::NotFound)
    }
}

impl FilesystemAttrsProvider for TestFs {
    fn attrs_fs(&mut self) -> Option<&mut dyn FilesystemAttrsFs> {
        if self.keeps_attrs {
            Some(self)
        } else {
            None
        }
    }
}
