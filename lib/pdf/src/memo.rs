//! A memo of the objects a document reads as nodes of a graph — functions
//! stitched of functions, visibility expressions of expressions — so one
//! shared however many times over is read once, and what reading it came to
//! never depends on where it was first met.

use core::cell::RefCell;

use tairix_collections::OrderedMap;

use crate::object::Object;
use crate::PdfError;

/// What a node read to.
pub(crate) trait Node: Clone {
    /// How many levels it nests beneath itself.
    fn nested(&self) -> usize;
}

/// The nodes of one kind a document has read, by object number.
pub(crate) struct Memo<T>(RefCell<OrderedMap<u32, Outcome<T>>>);

impl<T> Default for Memo<T> {
    fn default() -> Self {
        Self(RefCell::new(OrderedMap::new()))
    }
}

/// What reading a node came to.
#[derive(Clone)]
pub(crate) enum Outcome<T> {
    /// It is being read: met again meanwhile, it contains itself.
    Reading,
    /// What it read to, and how many nodes it is built of, one it shares
    /// counted each time it is met.
    Read(T, usize),
    /// Damage, wherever it is met.
    Damaged(PdfError),
    /// It nests deeper than this many levels: refused where no more remain,
    /// and read again where more do.
    Deeper(usize),
}

/// The bounds one walk down a graph is held to.
pub(crate) struct Bounds {
    /// The deepest a node may be met.
    pub(crate) depth: usize,
    /// The most nodes one walk meets, one shared counted each time.
    pub(crate) nodes: usize,
}

/// One walk down a graph from the node it began at.
#[derive(Default)]
pub(crate) struct Walk {
    met: usize,
    /// Whether a node was refused for nesting past the levels left where it
    /// was met, which says nothing of it met nearer the top.
    deep: bool,
}

impl Walk {
    /// Refuse a node for nesting past the levels left where it is met.
    pub(crate) fn too_deep(&mut self) -> PdfError {
        self.deep = true;
        PdfError::Malformed
    }
}

impl<T: Node> Memo<T> {
    /// The node `object` is, met `depth` levels down `walk` within `bounds`,
    /// read by `read` unless it is an object whose reading is already known.
    /// Every node it is built of is counted against `bounds.nodes` whether it
    /// is read or known, so whether a walk fits never depends on what was
    /// read before it.
    ///
    /// # Errors
    ///
    /// [`PdfError::TooComplex`] past `bounds.nodes`; [`PdfError::Malformed`]
    /// for a node that contains itself or nests past `bounds.depth` where it
    /// is met; otherwise what reading it came to.
    pub(crate) fn node(
        &self,
        object: &Object,
        depth: usize,
        bounds: &Bounds,
        walk: &mut Walk,
        read: impl FnOnce(&mut Walk) -> Result<T, PdfError>,
    ) -> Result<T, PdfError> {
        let before = walk.met;
        walk.met += 1;
        if walk.met > bounds.nodes {
            return Err(PdfError::TooComplex);
        }
        let Some(levels) = bounds.depth.checked_sub(depth) else {
            return Err(walk.too_deep());
        };
        let Some(reference) = object.as_reference() else {
            return read(walk);
        };
        match self.outcome(reference.num) {
            Some(Outcome::Read(node, built)) if node.nested() <= levels => {
                walk.met = before.saturating_add(built);
                if walk.met > bounds.nodes {
                    return Err(PdfError::TooComplex);
                }
                return Ok(node);
            }
            Some(Outcome::Read(..)) => return Err(walk.too_deep()),
            Some(Outcome::Deeper(known)) if known >= levels => return Err(walk.too_deep()),
            Some(Outcome::Damaged(error)) => return Err(error),
            Some(Outcome::Reading) => return Err(PdfError::Malformed),
            Some(Outcome::Deeper(_)) | None => {}
        }
        self.reading(reference.num)?;
        let read = read(walk);
        let built = walk.met.saturating_sub(before);
        // A bound passed or the heap refused says nothing of the node, so it
        // may be read again.
        let outcome = match &read {
            Ok(node) => Some(Outcome::Read(node.clone(), built)),
            Err(PdfError::TooComplex | PdfError::OutOfMemory) => None,
            Err(_) if walk.deep => Some(Outcome::Deeper(levels)),
            Err(error) => Some(Outcome::Damaged(*error)),
        };
        self.settle(reference.num, outcome);
        read
    }

    /// What reading `num` came to, where it was begun.
    pub(crate) fn outcome(&self, num: u32) -> Option<Outcome<T>> {
        self.0.try_borrow().ok()?.get(&num).cloned()
    }

    /// Mark `num` as being read.
    fn reading(&self, num: u32) -> Result<(), PdfError> {
        self.0
            .try_borrow_mut()
            .map_err(|_| PdfError::Malformed)?
            .try_insert(num, Outcome::Reading)?;
        Ok(())
    }

    /// Settle what reading `num` came to, or forget it was begun: never
    /// refused, as `num` is already held.
    fn settle(&self, num: u32, outcome: Option<Outcome<T>>) {
        let Ok(mut held) = self.0.try_borrow_mut() else {
            return;
        };
        match outcome {
            Some(outcome) => {
                if let Some(slot) = held.get_mut(&num) {
                    *slot = outcome;
                }
            }
            None => {
                held.remove(&num);
            }
        }
    }

    /// How many objects it holds an outcome for.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.0.borrow().len()
    }
}
