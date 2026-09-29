//! The document: a byte sequence held as pieces of shared, immutable chunks
//! and arranged in an implicit treap.
//!
//! Loaded bytes stay in the chunks they were read into; typed text is
//! appended to one *active* chunk. A node summarises how its subtree's bytes
//! break into lines, so an edit, an offset's line, a line's offset and the
//! rows of the grid before a line are all logarithmic in the number of
//! pieces. A snapshot seals the active chunk's bytes and lists the pieces, so
//! a save reads the document on another thread while editing carries on.
//!
//! Every allocation an edit needs is made before the edit changes anything:
//! the nodes live in one arena whose room is reserved first, so a refused
//! edit leaves the document exactly as it was.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ops::{ControlFlow, Range};

use tairix_rng::{NonCryptoRng, RandU64};
use tairix_util::lanes;

/// Longest piece the store holds, loaded or typed: the most a split or a
/// line lookup ever scans.
pub const MAX_PIECE: usize = 4096;

/// Capacity of each chunk typed or pasted text is appended to.
pub const ACTIVE_CHUNK: usize = 64 * 1024;

/// Most bytes one step of a long job over a document — a search, a
/// conversion, a load — goes through, so the worker carrying it is never
/// held long from the next job, and a job no longer wanted is put down
/// between steps.
pub const STEP_BYTES: usize = 16 * 1024 * 1024;

/// How far apart a long line's rows of the grid start: a line longer than
/// this continues on further rows, each starting this much further in, drawn
/// back to the start of a character it would otherwise split — so a row holds
/// at most three bytes more.
///
/// A containment bound on the work a row costs to lay out, hit-test or draw,
/// not a capacity: every row is measured from its own start, so nothing done
/// to a row reads past it however long its line is.
pub const MAX_ROW_BYTES: usize = 8 * 1024;

// A line a piece holds whole takes one row, which is what lets a piece's
// shape leave its inner lines uncounted.
const _: () = assert!(MAX_PIECE <= MAX_ROW_BYTES);

/// How many rows of the grid a line of `len` bytes, less its terminator,
/// takes.
#[must_use]
pub const fn rows_of(len: usize) -> usize {
    len.saturating_sub(1) / MAX_ROW_BYTES + 1
}

/// The allocator refused room for data the document would have held; the
/// document is as it was.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct OutOfMemory;

/// One run of bytes in one chunk, and how it breaks into lines.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Piece {
    chunk: u32,
    start: u32,
    len: u32,
    newlines: u32,
    /// Bytes before the first line feed; all of them when there is none.
    head: u32,
    /// Bytes after the last line feed; all of them when there is none.
    tail: u32,
    /// The byte before the first line feed is a CR.
    head_cr: bool,
    /// The last byte is a CR.
    last_cr: bool,
}

impl Piece {
    /// The piece of `bytes`, at most [`MAX_PIECE`] of them, lying at `start`
    /// in `chunk`.
    fn new(chunk: u32, start: u32, bytes: &[u8]) -> Self {
        Self::shaped(chunk, start, Shape::of_piece(bytes))
    }

    /// The piece at `start` in `chunk` whose bytes are shaped `shape`.
    fn shaped(chunk: u32, start: u32, shape: Shape) -> Self {
        let narrow = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
        Self {
            chunk,
            start,
            len: narrow(shape.bytes),
            newlines: narrow(shape.newlines),
            head: narrow(shape.head),
            tail: narrow(shape.tail),
            head_cr: shape.head_cr,
            last_cr: shape.last_cr,
        }
    }

    /// How many bytes the piece covers.
    #[must_use]
    pub(crate) const fn len(self) -> usize {
        self.len as usize
    }

    fn shape(self) -> Shape {
        Shape {
            bytes: self.len as usize,
            newlines: self.newlines as usize,
            head: self.head as usize,
            tail: self.tail as usize,
            extra: 0,
            head_cr: self.head_cr,
            last_cr: self.last_cr,
        }
    }

    /// The shape of the piece's bytes up to and including its first line
    /// feed; it must hold one.
    fn first_line(self) -> Shape {
        Shape {
            bytes: self.head as usize + 1,
            newlines: 1,
            head: self.head as usize,
            tail: 0,
            extra: 0,
            head_cr: self.head_cr,
            last_cr: false,
        }
    }

    /// This piece grown by the bytes shaped `more` that follow it in its
    /// chunk; together they are at most [`MAX_PIECE`].
    fn grown(self, more: Shape) -> Self {
        Self::shaped(self.chunk, self.start, self.shape().then(more))
    }
}

/// How a run of bytes breaks into lines: what the rows of the lines it holds
/// whole come to, and enough about its two ends to join it to its
/// neighbours.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Shape {
    bytes: usize,
    newlines: usize,
    /// Bytes before the first line feed; all of them when there is none.
    head: usize,
    /// Bytes after the last line feed; all of them when there is none.
    tail: usize,
    /// Rows beyond one each that the lines between two of its feeds take.
    extra: usize,
    /// The byte before the first line feed is a CR.
    head_cr: bool,
    /// The last byte is a CR.
    last_cr: bool,
}

impl Shape {
    const EMPTY: Self = Self {
        bytes: 0,
        newlines: 0,
        head: 0,
        tail: 0,
        extra: 0,
        head_cr: false,
        last_cr: false,
    };

    /// The shape of a piece's `bytes`, whose inner lines are all shorter
    /// than a row.
    fn of_piece(bytes: &[u8]) -> Self {
        let len = bytes.len();
        let last_cr = bytes.last() == Some(&b'\r');
        let (newlines, feeds) = lanes::span(bytes, b'\n');
        let Some((first, last)) = feeds else {
            return Self {
                bytes: len,
                head: len,
                tail: len,
                last_cr,
                ..Self::EMPTY
            };
        };
        Self {
            bytes: len,
            newlines,
            head: first,
            tail: len - last - 1,
            extra: 0,
            head_cr: first > 0 && bytes[first - 1] == b'\r',
            last_cr,
        }
    }

    /// This run followed by `next`.
    fn then(self, next: Self) -> Self {
        let bytes = self.bytes + next.bytes;
        if next.newlines == 0 {
            return Self {
                bytes,
                head: if self.newlines == 0 { bytes } else { self.head },
                tail: self.tail + next.bytes,
                last_cr: if next.bytes == 0 {
                    self.last_cr
                } else {
                    next.last_cr
                },
                ..self
            };
        }
        // The byte before `next`'s first feed, which may be this run's last.
        let cr = if next.head == 0 {
            self.last_cr
        } else {
            next.head_cr
        };
        if self.newlines == 0 {
            return Self {
                bytes,
                head: self.bytes + next.head,
                head_cr: cr,
                ..next
            };
        }
        let joined = (self.tail + next.head).saturating_sub(usize::from(cr));
        Self {
            bytes,
            newlines: self.newlines + next.newlines,
            head: self.head,
            tail: next.tail,
            extra: self.extra + next.extra + rows_of(joined) - 1,
            head_cr: self.head_cr,
            last_cr: next.last_cr,
        }
    }

    /// The rows the lines ending in this run take: every line it touches but
    /// the one after its last feed.
    fn ended_rows(self) -> usize {
        if self.newlines == 0 {
            return 0;
        }
        let first = self.head.saturating_sub(usize::from(self.head_cr));
        self.newlines + self.extra + rows_of(first) - 1
    }
}

/// One reversible edit: what it removed and what it put in its place.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Change {
    /// Where the edit happened.
    pub at: usize,
    removed: Vec<Piece>,
    inserted: Vec<Piece>,
}

impl Change {
    /// How many bytes the edit removed.
    #[must_use]
    pub fn removed_len(&self) -> usize {
        self.removed.iter().map(|piece| piece.len()).sum()
    }

    /// How many bytes the edit inserted.
    #[must_use]
    pub fn inserted_len(&self) -> usize {
        self.inserted.iter().map(|piece| piece.len()).sum()
    }

    /// Fold `next` into this change when it continues it — an insertion
    /// right where this one's insertion ended — so a typing run undoes as
    /// one; otherwise hand it back.
    ///
    /// # Errors
    ///
    /// `next` itself, when it does not continue this change.
    pub fn absorb(&mut self, next: Self) -> Result<(), Self> {
        if !next.removed.is_empty() || next.at != self.at + self.inserted_len() {
            return Err(next);
        }
        for piece in next.inserted {
            match self.inserted.last_mut() {
                Some(last)
                    if last.chunk == piece.chunk
                        && last.start + last.len == piece.start
                        && last.len() + piece.len() <= MAX_PIECE =>
                {
                    *last = last.grown(piece.shape());
                }
                _ => self.inserted.push(piece),
            }
        }
        Ok(())
    }
}

/// Bytes read in order from an offset: the document on the loop, a
/// snapshot of it on a worker.
pub trait Source {
    /// How many bytes there are.
    fn len(&self) -> usize;

    /// Whether there are none.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Visit the bytes from `from` on, a slice at a time, for as long as
    /// `visit` continues.
    fn walk(&self, from: usize, visit: impl FnMut(&[u8]) -> ControlFlow<()>);
}

impl Source for Document {
    fn len(&self) -> usize {
        Document::len(self)
    }

    fn walk(&self, from: usize, visit: impl FnMut(&[u8]) -> ControlFlow<()>) {
        Document::walk(self, from, visit);
    }
}

impl Source for Snapshot {
    fn len(&self) -> usize {
        Snapshot::len(self)
    }

    fn walk(&self, from: usize, visit: impl FnMut(&[u8]) -> ControlFlow<()>) {
        Snapshot::walk(self, from, visit);
    }
}

/// The document's bytes frozen at one moment, readable from another thread.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    chunks: Vec<Chunk>,
    pieces: Vec<Piece>,
    /// Where each piece ends, so a read finds its first piece by search.
    ends: Vec<usize>,
}

impl Snapshot {
    /// How many bytes the snapshot holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.ends.last().copied().unwrap_or(0)
    }

    /// Whether the snapshot is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Hand `write` the snapshot's bytes in runs as long as `run`'s capacity,
    /// gathered through it: a few large writes where [`walk`](Self::walk)
    /// would make one per piece. A `run` with no capacity hands each piece
    /// over as it is. The first refusal stops the walk and is answered.
    ///
    /// # Errors
    ///
    /// What `write` refused with.
    pub fn gather<E>(
        &self,
        run: &mut Vec<u8>,
        mut write: impl FnMut(&[u8]) -> Result<(), E>,
    ) -> Result<(), E> {
        let limit = run.capacity();
        run.clear();
        let mut result = Ok(());
        self.walk(0, |mut bytes| {
            if limit == 0 {
                result = write(bytes);
                return if result.is_ok() {
                    ControlFlow::Continue(())
                } else {
                    ControlFlow::Break(())
                };
            }
            while !bytes.is_empty() {
                let take = bytes.len().min(limit - run.len());
                run.extend_from_slice(&bytes[..take]);
                bytes = &bytes[take..];
                if run.len() == limit {
                    result = write(run);
                    run.clear();
                    if result.is_err() {
                        return ControlFlow::Break(());
                    }
                }
            }
            ControlFlow::Continue(())
        });
        result?;
        if !run.is_empty() {
            write(run)?;
            run.clear();
        }
        Ok(())
    }

    /// Visit the snapshot's bytes from `from` on, a slice at a time, for as
    /// long as `visit` continues.
    pub fn walk(&self, from: usize, mut visit: impl FnMut(&[u8]) -> ControlFlow<()>) {
        let first = self.ends.partition_point(|&end| end <= from);
        let mut skip = from - first.checked_sub(1).map_or(0, |before| self.ends[before]);
        for piece in &self.pieces[first.min(self.pieces.len())..] {
            let Some(Some(chunk)) = self.chunks.get(piece.chunk as usize) else {
                return;
            };
            let bytes = &chunk[piece.start as usize + skip..(piece.start + piece.len) as usize];
            skip = 0;
            if visit(bytes).is_break() {
                return;
            }
        }
    }
}

/// Where one line sits: its first byte, the end of its content (before its
/// terminator), and the first byte of the next line.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct LineBounds {
    /// The line's first byte.
    pub start: usize,
    /// One past the line's last content byte: the terminator's LF, or its CR
    /// when it ends CRLF.
    pub end: usize,
    /// The next line's first byte; the document's length for the last line.
    pub next: usize,
}

/// A sealed chunk, or `None` for one nothing names any longer and whose
/// bytes were let go; its index stays, since pieces name chunks by index.
type Chunk = Option<Arc<Vec<u8>>>;

/// A node's place in the arena, or `None` for the empty tree.
type Tree = Option<u32>;

/// Nodes a replacement may make beyond the pieces it puts in: each of its
/// two splits can cut a piece in two.
const SPLIT_NODES: usize = 2;

struct Node {
    piece: Piece,
    priority: u32,
    /// The shape of the node's whole subtree.
    shape: Shape,
    left: Tree,
    right: Tree,
}

/// The treap's nodes, in one arena whose room an edit reserves before it
/// changes anything.
#[derive(Default)]
struct Nodes {
    slots: Vec<Node>,
    /// Slots whose nodes were released, reused before the arena grows. Its
    /// room covers every slot, so releasing never allocates.
    free: Vec<u32>,
}

impl Nodes {
    /// Room for `more` nodes to be made without allocating.
    fn reserve(&mut self, more: usize) -> Result<(), OutOfMemory> {
        let fresh = more.saturating_sub(self.free.len());
        let slots = self.slots.len().checked_add(fresh).ok_or(OutOfMemory)?;
        u32::try_from(slots).map_err(|_| OutOfMemory)?;
        self.slots.try_reserve(fresh).map_err(|_| OutOfMemory)?;
        self.free
            .try_reserve(slots.saturating_sub(self.free.len()))
            .map_err(|_| OutOfMemory)
    }

    /// A leaf for `piece`, in room [`reserve`](Self::reserve) made.
    fn make(&mut self, piece: Piece, priority: u32) -> u32 {
        let node = Node {
            piece,
            priority,
            shape: piece.shape(),
            left: None,
            right: None,
        };
        if let Some(id) = self.free.pop() {
            self.slots[id as usize] = node;
            id
        } else {
            let id = u32::try_from(self.slots.len()).unwrap_or(u32::MAX);
            self.slots.push(node);
            id
        }
    }

    /// Put every node of `tree` back for reuse.
    fn release(&mut self, tree: Tree) {
        if let Some(id) = tree {
            let (left, right) = (self.node(id).left, self.node(id).right);
            self.release(left);
            self.release(right);
            self.free.push(id);
        }
    }

    fn node(&self, id: u32) -> &Node {
        &self.slots[id as usize]
    }

    fn node_mut(&mut self, id: u32) -> &mut Node {
        &mut self.slots[id as usize]
    }

    fn shape(&self, tree: Tree) -> Shape {
        tree.map_or(Shape::EMPTY, |id| self.node(id).shape)
    }

    fn bytes(&self, tree: Tree) -> usize {
        self.shape(tree).bytes
    }

    fn newlines(&self, tree: Tree) -> usize {
        self.shape(tree).newlines
    }

    /// Reshape `id`'s subtree from its piece and its children.
    fn update(&mut self, id: u32) {
        let node = self.node(id);
        let shape = self
            .shape(node.left)
            .then(node.piece.shape())
            .then(self.shape(node.right));
        self.node_mut(id).shape = shape;
    }

    /// Join two treaps, every byte of `a` before every byte of `b`.
    fn merge(&mut self, a: Tree, b: Tree) -> Tree {
        match (a, b) {
            (None, tree) | (tree, None) => tree,
            (Some(a), Some(b)) => {
                if self.node(a).priority >= self.node(b).priority {
                    let right = self.node_mut(a).right.take();
                    let merged = self.merge(right, Some(b));
                    self.node_mut(a).right = merged;
                    self.update(a);
                    Some(a)
                } else {
                    let left = self.node_mut(b).left.take();
                    let merged = self.merge(Some(a), left);
                    self.node_mut(b).left = merged;
                    self.update(b);
                    Some(b)
                }
            }
        }
    }

    /// A treap of `pieces`, in order, in reserved room: the Cartesian tree
    /// by the priorities `rng` draws, built left to right with its rightmost
    /// spine threaded through the spine nodes' own right links, so the build
    /// needs no stack.
    fn cartesian(
        &mut self,
        pieces: impl IntoIterator<Item = Piece>,
        rng: &mut NonCryptoRng,
    ) -> Tree {
        let mut top: Tree = None;
        for piece in pieces.into_iter().filter(|piece| piece.len > 0) {
            let id = self.make(piece, rng.next_u32());
            let priority = self.node(id).priority;
            let mut last: Tree = None;
            while let Some(below) = top.filter(|&spine| self.node(spine).priority < priority) {
                top = self.node(below).right;
                self.node_mut(below).right = last;
                self.update(below);
                last = Some(below);
            }
            let node = self.node_mut(id);
            node.left = last;
            node.right = top;
            top = Some(id);
        }
        let mut root: Tree = None;
        while let Some(spine) = top {
            top = self.node(spine).right;
            self.node_mut(spine).right = root;
            self.update(spine);
            root = Some(spine);
        }
        root
    }

    /// Every piece of `tree`, in order, onto `out`.
    fn collect(&self, tree: Tree, out: &mut Vec<Piece>) {
        if let Some(id) = tree {
            let node = self.node(id);
            self.collect(node.left, out);
            out.push(node.piece);
            self.collect(node.right, out);
        }
    }

    /// Mark every chunk a piece of `tree` names.
    fn name_chunks(&self, tree: Tree, named: &mut [bool]) {
        if let Some(id) = tree {
            let node = self.node(id);
            if let Some(slot) = named.get_mut(node.piece.chunk as usize) {
                *slot = true;
            }
            self.name_chunks(node.left, named);
            self.name_chunks(node.right, named);
        }
    }

    /// The last piece of `tree`.
    fn last_piece(&self, tree: Tree) -> Option<Piece> {
        let mut node = self.node(tree?);
        while let Some(right) = node.right {
            node = self.node(right);
        }
        Some(node.piece)
    }
}

/// The chunks pieces point into: every sealed one, and the active one,
/// whose id is the next after the sealed.
struct Store<'a> {
    chunks: &'a [Chunk],
    active: &'a [u8],
}

impl Store<'_> {
    fn bytes(&self, piece: Piece) -> &[u8] {
        let chunk: &[u8] = match self.chunks.get(piece.chunk as usize) {
            Some(sealed) => sealed.as_deref().map_or(&[], Vec::as_slice),
            None => self.active,
        };
        let (start, end) = (piece.start as usize, (piece.start + piece.len) as usize);
        chunk.get(start..end).unwrap_or(&[])
    }
}

/// How many line feeds `bytes` holds.
pub(crate) fn count_newlines(bytes: &[u8]) -> usize {
    lanes::count(bytes, b'\n')
}

/// Onto `out`, the pieces `span` — the pieces from byte `base` up to the end
/// of the last of `matches` — becomes with each match replaced by `copy`:
/// what lies between the matches kept, cut where a match ends or starts
/// inside a piece. A match overlapping the one before it is skipped.
fn rebuild(
    store: &Store<'_>,
    span: &[Piece],
    matches: &[Range<usize>],
    base: usize,
    copy: &[Piece],
    out: &mut Vec<Piece>,
) {
    let mut pieces = span.iter().copied();
    let mut current = pieces.next();
    // Move `n` bytes on through the span, putting what they cover onto `out`
    // when `keep` says to.
    let mut step = |n: usize, keep: bool, current: &mut Option<Piece>, out: &mut Vec<Piece>| {
        let mut left = n;
        while let Some(piece) = current.filter(|_| left > 0) {
            if piece.len() <= left {
                left -= piece.len();
                if keep {
                    out.push(piece);
                }
                *current = pieces.next();
            } else {
                let (head, tail) = cut(store, piece, left);
                if keep {
                    out.push(head);
                }
                *current = Some(tail);
                left = 0;
            }
        }
    };
    let mut at = 0;
    for range in matches {
        let start = range.start.saturating_sub(base);
        if start < at {
            continue;
        }
        let end = range.end.saturating_sub(base).max(start);
        step(start - at, true, &mut current, out);
        out.extend_from_slice(copy);
        step(end - start, false, &mut current, out);
        at = end;
    }
    out.extend(current);
    out.extend(pieces);
}

/// Cut `piece` `offset` bytes in.
fn cut(store: &Store<'_>, piece: Piece, offset: usize) -> (Piece, Piece) {
    let bytes = store.bytes(piece);
    let (head, tail) = bytes.split_at(offset.min(bytes.len()));
    let tail_start = piece.start + u32::try_from(head.len()).unwrap_or(piece.len);
    (
        Piece::new(piece.chunk, piece.start, head),
        Piece::new(piece.chunk, tail_start, tail),
    )
}

/// The document.
pub struct Document {
    root: Tree,
    nodes: Nodes,
    chunks: Vec<Chunk>,
    active: Vec<u8>,
    rng: NonCryptoRng,
}

impl core::fmt::Debug for Document {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Document")
            .field("len", &self.len())
            .field("lines", &self.line_count())
            .finish_non_exhaustive()
    }
}

impl Default for Document {
    fn default() -> Self {
        Self::new()
    }
}

/// A fixed seed: a treap's shape needs priorities independent of the
/// operations, not unpredictable ones.
const PRIORITY_SEED: u64 = 0x7e57_ed17_0d0c_0001;

impl Document {
    /// An empty document.
    #[must_use]
    pub fn new() -> Self {
        Self {
            root: None,
            nodes: Nodes::default(),
            chunks: Vec::new(),
            active: Vec::new(),
            rng: NonCryptoRng::seed_from_u64(PRIORITY_SEED),
        }
    }

    /// A document holding `chunks`, in order — what a load read.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`] when the piece index cannot be held.
    pub fn from_chunks(chunks: Vec<Vec<u8>>) -> Result<Self, OutOfMemory> {
        let mut document = Self::new();
        let pieces = document.adopt(chunks)?;
        document.nodes.reserve(pieces.len())?;
        document.root = document.nodes.cartesian(pieces, &mut document.rng);
        Ok(document)
    }

    /// How many bytes the document holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.nodes.bytes(self.root)
    }

    /// Whether the document is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.root.is_none()
    }

    /// How many lines the document has: one more than its line feeds.
    #[must_use]
    pub fn line_count(&self) -> usize {
        self.nodes.newlines(self.root) + 1
    }

    /// How many rows of the grid the document's lines take.
    #[must_use]
    pub fn row_count(&self) -> usize {
        let shape = self.nodes.shape(self.root);
        shape.ended_rows() + rows_of(shape.tail)
    }

    /// The row line `line` starts on, counted from the document's first; the
    /// row count for a line past the last.
    #[must_use]
    pub fn first_row(&self, line: usize) -> usize {
        if line == 0 {
            return 0;
        }
        let mut tree = self.root;
        let (mut nth, mut before) = (line, Shape::EMPTY);
        while let Some(id) = tree {
            let node = self.nodes.node(id);
            let left = self.nodes.shape(node.left);
            if nth <= left.newlines {
                tree = node.left;
                continue;
            }
            nth -= left.newlines;
            before = before.then(left);
            let own = node.piece.newlines as usize;
            if nth <= own {
                // Every line after the piece's first feed is shorter than a row.
                return before.then(node.piece.first_line()).ended_rows() + nth - 1;
            }
            nth -= own;
            before = before.then(node.piece.shape());
            tree = node.right;
        }
        self.row_count()
    }

    /// The line row `row` falls in, and the row that line starts on; the
    /// last line for a row past the end.
    #[must_use]
    pub fn line_of_row(&self, row: usize) -> (usize, usize) {
        let mut tree = self.root;
        let mut before = Shape::EMPTY;
        while let Some(id) = tree {
            let node = self.nodes.node(id);
            let left = before.then(self.nodes.shape(node.left));
            if left.ended_rows() > row {
                tree = node.left;
                continue;
            }
            let piece = node.piece;
            if piece.newlines > 0 {
                let ended = left.then(piece.first_line()).ended_rows();
                if ended > row {
                    return (left.newlines, left.ended_rows());
                }
                // Each line ended by the piece's later feeds takes one row.
                let within = row - ended;
                if within < piece.newlines as usize - 1 {
                    return (left.newlines + 1 + within, row);
                }
            }
            before = left.then(piece.shape());
            tree = node.right;
        }
        (before.newlines, before.ended_rows())
    }

    fn store(&self) -> Store<'_> {
        Store {
            chunks: &self.chunks,
            active: &self.active,
        }
    }

    /// The byte at `offset`.
    #[must_use]
    pub fn byte(&self, offset: usize) -> Option<u8> {
        let store = self.store();
        let mut tree = self.root;
        let mut offset = offset;
        while let Some(id) = tree {
            let node = self.nodes.node(id);
            let left = self.nodes.bytes(node.left);
            if offset < left {
                tree = node.left;
            } else if offset < left + node.piece.len() {
                return store.bytes(node.piece).get(offset - left).copied();
            } else {
                offset -= left + node.piece.len();
                tree = node.right;
            }
        }
        None
    }

    /// Where line `line` (0-based) starts; the document's length past the
    /// last line.
    #[must_use]
    pub fn line_start(&self, line: usize) -> usize {
        if line == 0 {
            return 0;
        }
        self.newline_offset(line).map_or(self.len(), |at| at + 1)
    }

    /// The offset of the `nth` line feed (1-based).
    fn newline_offset(&self, nth: usize) -> Option<usize> {
        let store = self.store();
        let mut tree = self.root;
        let (mut nth, mut base) = (nth, 0usize);
        while let Some(id) = tree {
            let node = self.nodes.node(id);
            let left = self.nodes.newlines(node.left);
            if nth <= left {
                tree = node.left;
                continue;
            }
            nth -= left;
            base += self.nodes.bytes(node.left);
            let own = node.piece.newlines as usize;
            if nth <= own {
                return lanes::nth(store.bytes(node.piece), b'\n', nth).map(|at| base + at);
            }
            nth -= own;
            base += node.piece.len();
            tree = node.right;
        }
        None
    }

    /// The 0-based line `offset` lies on.
    #[must_use]
    pub fn line_of(&self, offset: usize) -> usize {
        let store = self.store();
        let mut tree = self.root;
        let (mut offset, mut line) = (offset, 0usize);
        while let Some(id) = tree {
            let node = self.nodes.node(id);
            let left = self.nodes.bytes(node.left);
            if offset < left {
                tree = node.left;
                continue;
            }
            offset -= left;
            line += self.nodes.newlines(node.left);
            if offset < node.piece.len() {
                return line + count_newlines(&store.bytes(node.piece)[..offset]);
            }
            offset -= node.piece.len();
            line += node.piece.newlines as usize;
            tree = node.right;
        }
        line
    }

    /// Where line `line` sits.
    #[must_use]
    pub fn line_bounds(&self, line: usize) -> LineBounds {
        let start = self.line_start(line);
        match self.newline_offset(line + 1) {
            Some(feed) => {
                let end = if feed > start && self.byte(feed - 1) == Some(b'\r') {
                    feed - 1
                } else {
                    feed
                };
                LineBounds {
                    start,
                    end,
                    next: feed + 1,
                }
            }
            None => LineBounds {
                start,
                end: self.len(),
                next: self.len(),
            },
        }
    }

    /// Visit the document's bytes from `from` onward, a slice at a time, for
    /// as long as `visit` continues.
    pub fn walk(&self, from: usize, mut visit: impl FnMut(&[u8]) -> ControlFlow<()>) {
        let store = self.store();
        let _ = self.walk_tree(self.root, from, &store, &mut visit);
    }

    /// In-order visit of the slices of `tree` from `from`, stopping when
    /// `visit` breaks.
    fn walk_tree(
        &self,
        tree: Tree,
        from: usize,
        store: &Store<'_>,
        visit: &mut impl FnMut(&[u8]) -> ControlFlow<()>,
    ) -> ControlFlow<()> {
        let Some(id) = tree else {
            return ControlFlow::Continue(());
        };
        let node = self.nodes.node(id);
        if from >= node.shape.bytes {
            return ControlFlow::Continue(());
        }
        let left = self.nodes.bytes(node.left);
        if from < left {
            self.walk_tree(node.left, from, store, visit)?;
        }
        let own_end = left + node.piece.len();
        if from < own_end {
            let skip = from.saturating_sub(left);
            visit(&store.bytes(node.piece)[skip..])?;
        }
        self.walk_tree(node.right, from.saturating_sub(own_end), store, visit)
    }

    /// Append the bytes of `range` to `out`.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`] when `out` cannot grow to hold them; `out` holds what
    /// it did.
    pub fn copy_range(&self, range: Range<usize>, out: &mut Vec<u8>) -> Result<(), OutOfMemory> {
        let wanted = range.end.saturating_sub(range.start);
        out.try_reserve(wanted).map_err(|_| OutOfMemory)?;
        let mut left = wanted;
        self.walk(range.start, |slice| {
            let take = slice.len().min(left);
            out.extend_from_slice(&slice[..take]);
            left -= take;
            if left == 0 {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        });
        Ok(())
    }

    /// Replace `range` with `text`, answering the change for the history.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`] when the text or the index cannot be held; the
    /// document is unchanged.
    pub fn replace(&mut self, range: Range<usize>, text: &[u8]) -> Result<Change, OutOfMemory> {
        let (start, end) = self.clamp(range);
        let fresh = self.stage(text)?;
        let mut inserted = Vec::new();
        inserted
            .try_reserve_exact(text.len().div_ceil(MAX_PIECE) + fresh.len() + 1)
            .map_err(|_| OutOfMemory)?;
        self.chunks
            .try_reserve(fresh.len())
            .map_err(|_| OutOfMemory)?;
        self.nodes.reserve(inserted.capacity() + SPLIT_NODES)?;
        let coalesce = start == end && self.extends_active(start, text.len());
        let removed = self.room_for_removal(start..end)?;
        self.append(text, fresh, &mut inserted);
        Ok(self.exchange(start..end, inserted, removed, coalesce.then_some(text)))
    }

    /// Replace `range` with `chunks`, taken in whole rather than copied: what
    /// a whole-document conversion hands over.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`] when the index cannot be held; the document's text is
    /// unchanged.
    pub fn replace_owned(
        &mut self,
        range: Range<usize>,
        chunks: Vec<Vec<u8>>,
    ) -> Result<Change, OutOfMemory> {
        let (start, end) = self.clamp(range);
        let removed = self.room_for_removal(start..end)?;
        let pieces: usize = chunks
            .iter()
            .map(|chunk| chunk.len().div_ceil(MAX_PIECE))
            .sum();
        self.nodes.reserve(pieces + SPLIT_NODES)?;
        let inserted = self.adopt(chunks)?;
        Ok(self.exchange(start..end, inserted, removed, None))
    }

    /// Replace every one of `matches`, ascending and apart, with `text` as
    /// one change: the bytes between them stay in the pieces they were in,
    /// and every replacement names one copy of `text`. A match overlapping
    /// the one before it is left alone; no matches is no change.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`] when the text or the index cannot be held; the
    /// document is unchanged.
    pub fn replace_each(
        &mut self,
        matches: &[Range<usize>],
        text: &[u8],
    ) -> Result<Change, OutOfMemory> {
        let (Some(first), Some(last)) = (matches.first(), matches.last()) else {
            return Ok(Change {
                at: 0,
                removed: Vec::new(),
                inserted: Vec::new(),
            });
        };
        let (start, end) = self.clamp(first.start..last.end);
        let fresh = self.stage(text)?;
        let mut copy = Vec::new();
        copy.try_reserve_exact(text.len().div_ceil(MAX_PIECE) + fresh.len() + 1)
            .map_err(|_| OutOfMemory)?;
        self.chunks
            .try_reserve(fresh.len())
            .map_err(|_| OutOfMemory)?;
        let removed = self.room_for_removal(start..end)?;
        // Each match can cut a piece at both its ends, and puts in a copy.
        let most = matches
            .len()
            .saturating_mul(copy.capacity() + 2)
            .saturating_add(removed.capacity());
        let mut inserted = Vec::new();
        inserted.try_reserve_exact(most).map_err(|_| OutOfMemory)?;
        self.nodes.reserve(most.saturating_add(SPLIT_NODES))?;
        self.append(text, fresh, &mut copy);
        let root = self.root.take();
        let (left, rest) = self.split(root, start);
        let (middle, right) = self.split(rest, end - start);
        let mut removed = removed;
        self.nodes.collect(middle, &mut removed);
        self.nodes.release(middle);
        rebuild(
            &self.store(),
            &removed,
            matches,
            start,
            &copy,
            &mut inserted,
        );
        let body = self
            .nodes
            .cartesian(inserted.iter().copied(), &mut self.rng);
        let joined = self.nodes.merge(left, body);
        self.root = self.nodes.merge(joined, right);
        Ok(Change {
            at: start,
            removed,
            inserted,
        })
    }

    fn clamp(&self, range: Range<usize>) -> (usize, usize) {
        let len = self.len();
        let start = range.start.min(len);
        (start, range.end.clamp(start, len))
    }

    fn room_for_removal(&self, range: Range<usize>) -> Result<Vec<Piece>, OutOfMemory> {
        let mut removed = Vec::new();
        removed
            .try_reserve_exact(self.count_pieces(range))
            .map_err(|_| OutOfMemory)?;
        Ok(removed)
    }

    /// Put `inserted` in place of `range`, gathering what it replaced into
    /// `removed`; `typed` names text that grew the piece before `range` in
    /// place instead.
    fn exchange(
        &mut self,
        range: Range<usize>,
        inserted: Vec<Piece>,
        mut removed: Vec<Piece>,
        typed: Option<&[u8]>,
    ) -> Change {
        let root = self.root.take();
        let (left, rest) = self.split(root, range.start);
        let (middle, right) = self.split(rest, range.end - range.start);
        self.nodes.collect(middle, &mut removed);
        self.nodes.release(middle);
        let (left, body) = if let Some(text) = typed {
            (self.extend_last(left, text), None)
        } else {
            let body = self
                .nodes
                .cartesian(inserted.iter().copied(), &mut self.rng);
            (left, body)
        };
        let joined = self.nodes.merge(left, body);
        self.root = self.nodes.merge(joined, right);
        Change {
            at: range.start,
            removed,
            inserted,
        }
    }

    /// Take `chunks` in as shared chunks, answering their pieces in order.
    fn adopt(&mut self, chunks: Vec<Vec<u8>>) -> Result<Vec<Piece>, OutOfMemory> {
        let count: usize = chunks
            .iter()
            .map(|chunk| chunk.len().div_ceil(MAX_PIECE))
            .sum();
        let mut pieces = Vec::new();
        pieces.try_reserve_exact(count).map_err(|_| OutOfMemory)?;
        self.chunks
            .try_reserve(chunks.len() + 1)
            .map_err(|_| OutOfMemory)?;
        // The active chunk's pieces already name the index it will be sealed
        // at, so it is sealed before anything else takes a place.
        self.seal()?;
        for chunk in chunks.into_iter().filter(|chunk| !chunk.is_empty()) {
            let id = u32::try_from(self.chunks.len()).map_err(|_| OutOfMemory)?;
            for (index, part) in chunk.chunks(MAX_PIECE).enumerate() {
                let start = u32::try_from(index * MAX_PIECE).map_err(|_| OutOfMemory)?;
                pieces.push(Piece::new(id, start, part));
            }
            self.chunks.push(Some(Arc::new(chunk)));
        }
        Ok(pieces)
    }

    /// Make room to take every one of `changes` back out, so a group is
    /// undone whole or not at all.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`] when the room cannot be held; nothing has changed.
    pub fn room_to_revert(&mut self, changes: &[Change]) -> Result<(), OutOfMemory> {
        self.nodes.reserve(
            changes
                .iter()
                .map(|change| change.removed.len() + SPLIT_NODES)
                .sum(),
        )
    }

    /// Make room to put every one of `changes` back in, so a group is redone
    /// whole or not at all.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`] when the room cannot be held; nothing has changed.
    pub fn room_to_reapply(&mut self, changes: &[Change]) -> Result<(), OutOfMemory> {
        self.nodes.reserve(
            changes
                .iter()
                .map(|change| change.inserted.len() + SPLIT_NODES)
                .sum(),
        )
    }

    /// Undo `change`: take out what it inserted and put back what it removed,
    /// in the room [`room_to_revert`](Self::room_to_revert) made.
    pub fn revert(&mut self, change: &Change) {
        self.splice(change.at, change.inserted_len(), &change.removed);
    }

    /// Redo `change`: take out what it removed and put back what it inserted,
    /// in the room [`room_to_reapply`](Self::room_to_reapply) made.
    pub fn reapply(&mut self, change: &Change) {
        self.splice(change.at, change.removed_len(), &change.inserted);
    }

    /// Let go of every sealed chunk that neither the document nor one of
    /// `changes` — what the history can still undo or redo — names. A
    /// snapshot holding one keeps it alive until the snapshot is dropped.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`] when the tally cannot be held; nothing is let go.
    pub fn release_unnamed<'a>(
        &mut self,
        changes: impl IntoIterator<Item = &'a Change>,
    ) -> Result<(), OutOfMemory> {
        let named = tairix_util::fallible::filled(self.chunks.len(), false);
        let mut named = named.ok_or(OutOfMemory)?;
        self.nodes.name_chunks(self.root, &mut named);
        for change in changes {
            for piece in change.removed.iter().chain(&change.inserted) {
                if let Some(slot) = named.get_mut(piece.chunk as usize) {
                    *slot = true;
                }
            }
        }
        for (chunk, named) in self.chunks.iter_mut().zip(named) {
            if !named {
                *chunk = None;
            }
        }
        Ok(())
    }

    /// The document frozen as it is now.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`] when the piece list cannot be held.
    pub fn snapshot(&mut self) -> Result<Snapshot, OutOfMemory> {
        let mut pieces = Vec::new();
        pieces
            .try_reserve_exact(self.count_pieces(0..self.len()))
            .map_err(|_| OutOfMemory)?;
        let mut chunks = Vec::new();
        chunks
            .try_reserve_exact(self.chunks.len() + 1)
            .map_err(|_| OutOfMemory)?;
        let mut ends = Vec::new();
        ends.try_reserve_exact(pieces.capacity())
            .map_err(|_| OutOfMemory)?;
        self.seal()?;
        chunks.extend(self.chunks.iter().cloned());
        self.nodes.collect(self.root, &mut pieces);
        let mut end = 0;
        ends.extend(pieces.iter().map(|piece| {
            end += piece.len();
            end
        }));
        Ok(Snapshot {
            chunks,
            pieces,
            ends,
        })
    }

    /// The whole document, contiguous.
    #[cfg(test)]
    pub(crate) fn to_vec(&self) -> Result<Vec<u8>, OutOfMemory> {
        let mut out = Vec::new();
        self.copy_range(0..self.len(), &mut out)?;
        Ok(out)
    }

    /// Bytes the document's chunks hold room for, sealed and active.
    #[cfg(test)]
    pub(crate) fn held(&self) -> usize {
        self.chunks
            .iter()
            .flatten()
            .map(|chunk| chunk.capacity())
            .sum::<usize>()
            + self.active.capacity()
    }

    /// Freeze what the active chunk holds, so every piece points at shared,
    /// immutable bytes: a full chunk is moved, a part-filled one is copied
    /// out exactly, and it keeps its room for what is typed next.
    fn seal(&mut self) -> Result<(), OutOfMemory> {
        if self.active.is_empty() {
            return Ok(());
        }
        self.chunks.try_reserve(1).map_err(|_| OutOfMemory)?;
        if self.active.len() == self.active.capacity() {
            self.retire_active();
            return Ok(());
        }
        let mut sealed = Vec::new();
        sealed
            .try_reserve_exact(self.active.len())
            .map_err(|_| OutOfMemory)?;
        sealed.extend_from_slice(&self.active);
        self.chunks.push(Some(Arc::new(sealed)));
        self.active.clear();
        Ok(())
    }

    /// Move the full active chunk into the sealed set, in reserved room.
    fn retire_active(&mut self) {
        if !self.active.is_empty() {
            self.chunks
                .push(Some(Arc::new(core::mem::take(&mut self.active))));
        }
    }

    /// Room for `text` beyond what the active chunk has left: the chunks it
    /// will spill into, reserved before anything changes.
    fn stage(&mut self, text: &[u8]) -> Result<Vec<Vec<u8>>, OutOfMemory> {
        let room = self.active.capacity() - self.active.len();
        let spill = text.len().saturating_sub(room);
        let mut fresh = Vec::new();
        fresh
            .try_reserve_exact(spill.div_ceil(ACTIVE_CHUNK))
            .map_err(|_| OutOfMemory)?;
        let mut left = spill;
        while left > 0 {
            let mut chunk = Vec::new();
            chunk
                .try_reserve_exact(ACTIVE_CHUNK)
                .map_err(|_| OutOfMemory)?;
            fresh.push(chunk);
            left = left.saturating_sub(ACTIVE_CHUNK);
        }
        let chunk_ids = self.chunks.len() + fresh.len() + 1;
        u32::try_from(chunk_ids).map_err(|_| OutOfMemory)?;
        Ok(fresh)
    }

    /// Whether an insertion of `len` bytes at `at` can grow the piece
    /// ending there in place: it ends at the active chunk's end and the
    /// chunk has the room.
    fn extends_active(&self, at: usize, len: usize) -> bool {
        if len == 0 || self.active.len() + len > self.active.capacity() {
            return false;
        }
        let active = u32::try_from(self.chunks.len()).unwrap_or(u32::MAX);
        let fill = self.active.len();
        self.piece_ending_at(at).is_some_and(|piece| {
            piece.chunk == active
                && (piece.start + piece.len) as usize == fill
                && piece.len() + len <= MAX_PIECE
        })
    }

    /// The piece that ends exactly at `at`, if a piece does.
    fn piece_ending_at(&self, at: usize) -> Option<Piece> {
        let mut tree = self.root;
        let mut at = at;
        while let Some(id) = tree {
            let node = self.nodes.node(id);
            let left = self.nodes.bytes(node.left);
            let end = left + node.piece.len();
            if at <= left {
                tree = node.left;
            } else if at == end {
                return Some(node.piece);
            } else if at < end {
                return None;
            } else {
                at -= end;
                tree = node.right;
            }
        }
        None
    }

    /// Append `text` to the active chunk's room and then to the `fresh`
    /// chunks staged for the rest, putting the pieces it now occupies onto
    /// `pieces`, which has the room.
    fn append(&mut self, text: &[u8], fresh: Vec<Vec<u8>>, pieces: &mut Vec<Piece>) {
        let room = self.active.capacity() - self.active.len();
        let (now, mut rest) = text.split_at(room.min(text.len()));
        self.push_active(now, pieces);
        for chunk in fresh {
            self.retire_active();
            self.active = chunk;
            let (now, later) = rest.split_at(rest.len().min(self.active.capacity()));
            self.push_active(now, pieces);
            rest = later;
        }
    }

    /// Append `bytes`, which fit, to the active chunk, as pieces of at most
    /// [`MAX_PIECE`].
    fn push_active(&mut self, bytes: &[u8], pieces: &mut Vec<Piece>) {
        for part in bytes.chunks(MAX_PIECE) {
            pieces.push(Piece::new(
                u32::try_from(self.chunks.len()).unwrap_or(u32::MAX),
                u32::try_from(self.active.len()).unwrap_or(u32::MAX),
                part,
            ));
            self.active.extend_from_slice(part);
        }
    }

    /// Grow the last piece of `tree` by the `text` just appended after it.
    fn extend_last(&mut self, tree: Tree, text: &[u8]) -> Tree {
        let total = self.nodes.bytes(tree);
        let last = self.nodes.last_piece(tree).map_or(0, Piece::len);
        let (head, tail) = self.split(tree, total - last);
        if let Some(id) = tail {
            let node = self.nodes.node_mut(id);
            node.piece = node.piece.grown(Shape::of_piece(text));
            self.nodes.update(id);
        }
        self.nodes.merge(head, tail)
    }

    /// Replace `len` bytes at `at` with `pieces`, the history's own: none is
    /// new, so nothing is made but the nodes, in room already reserved.
    fn splice(&mut self, at: usize, len: usize, pieces: &[Piece]) {
        let root = self.root.take();
        let (left, rest) = self.split(root, at);
        let (middle, right) = self.split(rest, len);
        self.nodes.release(middle);
        let body = self.nodes.cartesian(pieces.iter().copied(), &mut self.rng);
        let joined = self.nodes.merge(left, body);
        self.root = self.nodes.merge(joined, right);
    }

    /// How many pieces overlap `range`.
    fn count_pieces(&self, range: Range<usize>) -> usize {
        fn count(nodes: &Nodes, tree: Tree, from: usize, to: usize) -> usize {
            let Some(id) = tree else {
                return 0;
            };
            let node = nodes.node(id);
            if from >= to || to == 0 || from >= node.shape.bytes {
                return 0;
            }
            let left = nodes.bytes(node.left);
            let own_end = left + node.piece.len();
            let mut total = 0;
            if from < left {
                total += count(nodes, node.left, from, to.min(left));
            }
            if from < own_end && to > left {
                total += 1;
            }
            if to > own_end {
                total += count(
                    nodes,
                    node.right,
                    from.saturating_sub(own_end),
                    to - own_end,
                );
            }
            total
        }
        count(&self.nodes, self.root, range.start, range.end)
    }

    /// Split `tree` at byte `at`: everything before, everything from. A
    /// piece straddling `at` is cut in two, its tail a node made in room
    /// already reserved.
    fn split(&mut self, tree: Tree, at: usize) -> (Tree, Tree) {
        let Some(id) = tree else {
            return (None, None);
        };
        let node = self.nodes.node(id);
        let left = self.nodes.bytes(node.left);
        let own_end = left + node.piece.len();
        if at <= left {
            let child = self.nodes.node_mut(id).left.take();
            let (before, after) = self.split(child, at);
            self.nodes.node_mut(id).left = after;
            self.nodes.update(id);
            (before, Some(id))
        } else if at >= own_end {
            let child = self.nodes.node_mut(id).right.take();
            let (before, after) = self.split(child, at - own_end);
            self.nodes.node_mut(id).right = before;
            self.nodes.update(id);
            (Some(id), after)
        } else {
            let (head, tail) = cut(&self.store(), node.piece, at - left);
            let right = self.nodes.node_mut(id).right.take();
            self.nodes.node_mut(id).piece = head;
            self.nodes.update(id);
            let tail = self.nodes.make(tail, self.rng.next_u32());
            (Some(id), self.nodes.merge(Some(tail), right))
        }
    }
}

#[cfg(test)]
#[path = "document_tests.rs"]
mod tests;
