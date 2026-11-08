//! A group's children: a persistent sequence held in chunks.
//!
//! Each chunk holds a bounded run of shared nodes and a summary of them —
//! their bounds, how many objects and path nodes they hold, how deep they
//! nest — so an edit copies the chunks it lands in and the list of chunks,
//! not every child, and a render or a hit test skips every chunk whose bounds
//! it does not meet.

use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_bezier::Bounds;
use tairix_collections::{try_arc, SharedSlice};
use tairix_util::fallible::{self, try_push};

use crate::node::Node;
use crate::DrawingError;

/// The children a chunk is built with.
const CHUNK_TARGET: usize = 64;

/// The most children a chunk holds before an insertion splits it.
const CHUNK_MAX: usize = 2 * CHUNK_TARGET;

/// The fewest children a chunk is left holding while it has a neighbour to
/// join.
const CHUNK_MIN: usize = CHUNK_TARGET / 2;

/// What a run of nodes holds.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Summary {
    /// The bounds of all of them, in the space they stand in.
    pub bounds: Bounds,
    /// How many objects they are, everything within them counted.
    pub objects: usize,
    /// How many path nodes they hold.
    pub path_nodes: usize,
    /// The pixels of the pictures they show, a picture counted for every
    /// object showing it.
    pub picture_pixels: u64,
    /// How deep the deepest of them nests: one for a shape or text.
    pub depth: usize,
}

impl Default for Summary {
    fn default() -> Self {
        Self {
            bounds: Bounds::EMPTY,
            objects: 0,
            path_nodes: 0,
            picture_pixels: 0,
            depth: 0,
        }
    }
}

impl Summary {
    pub(crate) fn of(node: &Node) -> Self {
        Self {
            bounds: node.bounds(),
            objects: node.objects(),
            path_nodes: node.path_nodes(),
            picture_pixels: node.picture_pixels(),
            depth: node.depth(),
        }
    }

    pub(crate) fn with(self, other: Self) -> Self {
        Self {
            bounds: self.bounds.union(other.bounds),
            objects: self.objects.saturating_add(other.objects),
            path_nodes: self.path_nodes.saturating_add(other.path_nodes),
            picture_pixels: self.picture_pixels.saturating_add(other.picture_pixels),
            depth: self.depth.max(other.depth),
        }
    }
}

#[derive(Debug)]
struct Chunk {
    nodes: Vec<Arc<Node>>,
    summary: Summary,
}

impl Chunk {
    fn new(nodes: Vec<Arc<Node>>) -> Self {
        let summary = nodes.iter().fold(Summary::default(), |summary, node| {
            summary.with(Summary::of(node))
        });
        Self { nodes, summary }
    }
}

/// A group's children, bottom first. Its chunks are shared, so children
/// clone, as the nodes holding them do, without allocating.
#[derive(Clone, Debug, Default)]
pub struct Children {
    chunks: SharedSlice<Arc<Chunk>>,
    /// The index of each chunk's first child.
    starts: SharedSlice<usize>,
    len: usize,
    summary: Summary,
}

impl PartialEq for Children {
    fn eq(&self, other: &Self) -> bool {
        self.len == other.len && self.iter().zip(other.iter()).all(|(a, b)| a == b)
    }
}

impl Children {
    /// No children.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            chunks: SharedSlice::new(),
            starts: SharedSlice::new(),
            len: 0,
            summary: Summary {
                bounds: Bounds::EMPTY,
                objects: 0,
                path_nodes: 0,
                picture_pixels: 0,
                depth: 0,
            },
        }
    }

    /// `node` alone, as children.
    ///
    /// # Errors
    ///
    /// [`DrawingError::OutOfMemory`] when it cannot be held.
    pub fn single(node: Arc<Node>) -> Result<Self, DrawingError> {
        let mut nodes = Vec::new();
        if !fallible::push(&mut nodes, node) {
            return Err(DrawingError::OutOfMemory);
        }
        Self::from_nodes(nodes)
    }

    /// `nodes`, bottom first, as children.
    ///
    /// # Errors
    ///
    /// [`DrawingError::OutOfMemory`] when they cannot be held.
    pub fn from_nodes(nodes: Vec<Arc<Node>>) -> Result<Self, DrawingError> {
        let mut laying = Laying::new(nodes.len().div_ceil(CHUNK_TARGET))?;
        laying.lay(nodes)?;
        laying.finish()
    }

    /// Children of `chunks`, their starts and summary worked out.
    fn of_chunks(chunks: Vec<Arc<Chunk>>) -> Result<Self, DrawingError> {
        let mut starts = Vec::new();
        if !fallible::reserve(&mut starts, chunks.len()) {
            return Err(DrawingError::OutOfMemory);
        }
        let mut len = 0;
        let mut summary = Summary::default();
        for chunk in &chunks {
            starts.push(len);
            len += chunk.nodes.len();
            summary = summary.with(chunk.summary);
        }
        Ok(Self {
            chunks: SharedSlice::try_from_vec(chunks)?,
            starts: SharedSlice::try_from_vec(starts)?,
            len,
            summary,
        })
    }

    /// How many children there are.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether there are none.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// What they hold together.
    #[must_use]
    pub const fn summary(&self) -> Summary {
        self.summary
    }

    /// The child at `index`, bottom first.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&Arc<Node>> {
        let (chunk, within) = self.locate(index)?;
        self.chunks[chunk].nodes.get(within)
    }

    /// The chunk holding child `index`, and its place there.
    fn locate(&self, index: usize) -> Option<(usize, usize)> {
        if index >= self.len {
            return None;
        }
        let chunk = self.starts.partition_point(|&start| start <= index) - 1;
        Some((chunk, index - self.starts[chunk]))
    }

    /// Every child, bottom first.
    #[must_use]
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &Arc<Node>> + '_ {
        self.chunks.iter().flat_map(|chunk| chunk.nodes.iter())
    }

    /// Every child whose bounds meet `area`, bottom first, with its index:
    /// whole chunks that cannot are passed over.
    pub fn meeting(&self, area: Bounds) -> impl Iterator<Item = (usize, &Arc<Node>)> + '_ {
        self.chunks
            .iter()
            .zip(self.starts.iter())
            .filter(move |(chunk, _)| chunk.summary.bounds.intersects(area))
            .flat_map(move |(chunk, &start)| {
                chunk
                    .nodes
                    .iter()
                    .enumerate()
                    .filter(move |(_, node)| node.bounds().intersects(area))
                    .map(move |(at, node)| (start + at, node))
            })
    }

    /// The index of the child `id` names, if one does.
    #[must_use]
    pub fn position(&self, id: crate::ObjectId) -> Option<usize> {
        self.iter().position(|node| node.id() == id)
    }

    /// The children as a list, each shared.
    ///
    /// # Errors
    ///
    /// [`DrawingError::OutOfMemory`] when the list cannot be held.
    pub fn to_vec(&self) -> Result<Vec<Arc<Node>>, DrawingError> {
        let mut list = Vec::new();
        if !fallible::reserve(&mut list, self.len) {
            return Err(DrawingError::OutOfMemory);
        }
        list.extend(self.iter().cloned());
        Ok(list)
    }

    /// These children with the one at `index` replaced by `node`.
    ///
    /// # Errors
    ///
    /// [`DrawingError::Invalid`] for an index past the end, and
    /// [`DrawingError::OutOfMemory`] when the copy cannot be held.
    pub fn replaced(&self, index: usize, node: Arc<Node>) -> Result<Self, DrawingError> {
        self.with_replaced(core::iter::once((index, node)))
    }

    /// These children with each node of `replacements` in place of the child
    /// at its index, the indices ascending. Only the chunks a replacement
    /// lands in are copied.
    ///
    /// # Errors
    ///
    /// [`DrawingError::Invalid`] for an index past the end or one not past
    /// the index before it, and [`DrawingError::OutOfMemory`] when the copy
    /// cannot be held.
    pub fn with_replaced(
        &self,
        replacements: impl IntoIterator<Item = (usize, Arc<Node>)>,
    ) -> Result<Self, DrawingError> {
        let mut replacements = replacements.into_iter().peekable();
        let mut chunks = Vec::new();
        if !fallible::reserve(&mut chunks, self.chunks.len()) {
            return Err(DrawingError::OutOfMemory);
        }
        let mut next = 0;
        for (chunk, &start) in self.chunks.iter().zip(self.starts.iter()) {
            let end = start + chunk.nodes.len();
            if replacements.peek().is_none_or(|(index, _)| *index >= end) {
                chunks.push(chunk.clone());
                continue;
            }
            let mut nodes = copied(&chunk.nodes, 0)?;
            while let Some((index, node)) = replacements.next_if(|(index, _)| *index < end) {
                if index < next {
                    return Err(DrawingError::Invalid);
                }
                next = index + 1;
                nodes[index - start] = node;
            }
            chunks.push(try_arc(Chunk::new(nodes))?);
        }
        if replacements.next().is_some() {
            return Err(DrawingError::Invalid);
        }
        Self::of_chunks(chunks)
    }

    /// These children with `node` inserted at `index`, `len()` appending it.
    ///
    /// # Errors
    ///
    /// [`DrawingError::Invalid`] for an index past the end, and
    /// [`DrawingError::OutOfMemory`] when the copy cannot be held.
    pub fn inserted(&self, index: usize, node: Arc<Node>) -> Result<Self, DrawingError> {
        self.with_inserted(core::iter::once((index, node)))
    }

    /// These children with `nodes` after the last of them, in order.
    ///
    /// # Errors
    ///
    /// [`DrawingError::OutOfMemory`] when the copy cannot be held.
    pub fn appended(
        &self,
        nodes: impl IntoIterator<Item = Arc<Node>>,
    ) -> Result<Self, DrawingError> {
        let end = self.len;
        self.with_inserted(nodes.into_iter().map(|node| (end, node)))
    }

    /// These children with each node of `insertions` put before the child
    /// now at its index, or after the last for `len()`, those given one index
    /// kept in the order given. Only the chunks an insertion lands in are
    /// copied.
    ///
    /// # Errors
    ///
    /// [`DrawingError::Invalid`] for an index past the end or one less than
    /// the index before it, and [`DrawingError::OutOfMemory`] when the copy
    /// cannot be held.
    pub fn with_inserted(
        &self,
        insertions: impl IntoIterator<Item = (usize, Arc<Node>)>,
    ) -> Result<Self, DrawingError> {
        let mut insertions = insertions.into_iter().peekable();
        let mut laying = Laying::new(self.chunks.len() + 1)?;
        let mut floor = 0;
        let mut take = |nodes: &mut Vec<Arc<Node>>, (index, node): (usize, Arc<Node>)| {
            if index < floor {
                return Err(DrawingError::Invalid);
            }
            floor = index;
            Ok(try_push(nodes, node)?)
        };
        let last = self.chunks.len().checked_sub(1);
        for (place, (chunk, &start)) in self.chunks.iter().zip(self.starts.iter()).enumerate() {
            let end = start + chunk.nodes.len();
            let closing = Some(place) == last;
            let due = |index: usize| index < end || (closing && index == end);
            if !insertions.peek().is_some_and(|(index, _)| due(*index)) {
                laying.share(chunk)?;
                continue;
            }
            let mut nodes = copied(&[], chunk.nodes.len())?;
            for (at, node) in chunk.nodes.iter().enumerate() {
                while let Some(insertion) = insertions.next_if(|(index, _)| *index <= start + at) {
                    take(&mut nodes, insertion)?;
                }
                try_push(&mut nodes, node.clone())?;
            }
            while let Some(insertion) = insertions.next_if(|(index, _)| closing && *index == end) {
                take(&mut nodes, insertion)?;
            }
            laying.lay(nodes)?;
        }
        if last.is_none() {
            let mut nodes = Vec::new();
            while let Some(insertion) = insertions.next_if(|(index, _)| *index == 0) {
                take(&mut nodes, insertion)?;
            }
            laying.lay(nodes)?;
        }
        if insertions.next().is_some() {
            return Err(DrawingError::Invalid);
        }
        laying.finish()
    }

    /// These children with the one at `index` removed.
    ///
    /// # Errors
    ///
    /// [`DrawingError::Invalid`] for an index past the end, and
    /// [`DrawingError::OutOfMemory`] when the copy cannot be held.
    pub fn removed(&self, index: usize) -> Result<Self, DrawingError> {
        self.without(&[index])
    }

    /// These children without those at `indices`, which ascend. Only the
    /// chunks a removal falls in are copied.
    ///
    /// # Errors
    ///
    /// [`DrawingError::Invalid`] for an index past the end or indices out of
    /// order, and [`DrawingError::OutOfMemory`] when the copy cannot be held.
    pub fn without(&self, indices: &[usize]) -> Result<Self, DrawingError> {
        let ascending = indices.windows(2).all(|pair| pair[0] < pair[1]);
        if !ascending || indices.last().is_some_and(|&last| last >= self.len) {
            return Err(DrawingError::Invalid);
        }
        if indices.is_empty() {
            return Ok(self.clone());
        }
        let mut laying = Laying::new(self.chunks.len())?;
        let mut rest = indices;
        for (chunk, &start) in self.chunks.iter().zip(self.starts.iter()) {
            let end = start + chunk.nodes.len();
            let (here, later) = rest.split_at(rest.partition_point(|&index| index < end));
            rest = later;
            if here.is_empty() {
                laying.share(chunk)?;
                continue;
            }
            let mut kept = copied(&[], chunk.nodes.len() - here.len())?;
            let mut gone = here.iter().map(|&index| index - start).peekable();
            for (at, node) in chunk.nodes.iter().enumerate() {
                if gone.next_if_eq(&at).is_none() {
                    kept.push(node.clone());
                }
            }
            laying.lay(kept)?;
        }
        laying.finish()
    }
}

/// A copy of `nodes` with room for `more`.
fn copied(nodes: &[Arc<Node>], more: usize) -> Result<Vec<Arc<Node>>, DrawingError> {
    let mut copy = Vec::new();
    if !fallible::reserve(&mut copy, nodes.len() + more) {
        return Err(DrawingError::OutOfMemory);
    }
    copy.extend(nodes.iter().cloned());
    Ok(copy)
}

/// Chunks laid end to end. A chunk passed through untouched is shared; a run
/// of nodes laid anew too short to stand alone waits to join what follows,
/// or what came before when nothing does, so removals leave no slivers.
struct Laying {
    chunks: Vec<Arc<Chunk>>,
    short: Vec<Arc<Node>>,
}

impl Laying {
    /// Room for `chunks` chunks.
    fn new(chunks: usize) -> Result<Self, DrawingError> {
        let mut laid = Vec::new();
        if !fallible::reserve(&mut laid, chunks) {
            return Err(DrawingError::OutOfMemory);
        }
        Ok(Self {
            chunks: laid,
            short: Vec::new(),
        })
    }

    /// `chunk` next, shared unless a short run is waiting to join it.
    fn share(&mut self, chunk: &Arc<Chunk>) -> Result<(), DrawingError> {
        if self.short.is_empty() {
            return Ok(try_push(&mut self.chunks, chunk.clone())?);
        }
        self.lay(copied(&chunk.nodes, 0)?)
    }

    /// `nodes`, laid anew, next.
    fn lay(&mut self, mut nodes: Vec<Arc<Node>>) -> Result<(), DrawingError> {
        if !self.short.is_empty() {
            let mut joined = core::mem::take(&mut self.short);
            if !fallible::reserve(&mut joined, nodes.len()) {
                return Err(DrawingError::OutOfMemory);
            }
            joined.append(&mut nodes);
            nodes = joined;
        }
        if nodes.len() < CHUNK_MIN {
            self.short = nodes;
            return Ok(());
        }
        self.split(nodes)
    }

    /// `nodes` as chunks of near equal size, none past [`CHUNK_MAX`].
    fn split(&mut self, nodes: Vec<Arc<Node>>) -> Result<(), DrawingError> {
        let total = nodes.len();
        let count = if total <= CHUNK_MAX {
            1
        } else {
            total.div_ceil(CHUNK_TARGET)
        };
        if !fallible::reserve(&mut self.chunks, count) {
            return Err(DrawingError::OutOfMemory);
        }
        if count == 1 {
            self.chunks.push(try_arc(Chunk::new(nodes))?);
            return Ok(());
        }
        let mut source = nodes.into_iter();
        for piece in 0..count {
            let size = total / count + usize::from(piece < total % count);
            let mut chunk = copied(&[], size)?;
            chunk.extend(source.by_ref().take(size));
            self.chunks.push(try_arc(Chunk::new(chunk))?);
        }
        Ok(())
    }

    /// The children laid, a short run left over joining the last chunk.
    fn finish(mut self) -> Result<Children, DrawingError> {
        let short = core::mem::take(&mut self.short);
        if !short.is_empty() {
            let joined = match self.chunks.pop() {
                Some(last) => {
                    let mut nodes = copied(&last.nodes, short.len())?;
                    nodes.extend(short);
                    nodes
                }
                None => short,
            };
            self.split(joined)?;
        }
        Children::of_chunks(self.chunks)
    }
}

#[cfg(test)]
#[path = "children_tests.rs"]
mod tests;
