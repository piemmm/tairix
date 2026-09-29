//! The renders a window has asked the desktop for and not yet been answered,
//! and the regions they land in.
//!
//! The desktop renders several pictures at once, so several may be pending,
//! each into its own region; a region is kept for the next picture once its
//! answer has landed, so regions are made only when the size the pictures are
//! drawn at moves or more renders run at once than before. It performs no I/O:
//! the region type is the caller's, and so is every request.

use alloc::vec::Vec;

use tairix_abi::window_ipc::PreviewSubject;
use tairix_abi::Errno;

use crate::pictures::PictureWanted;

/// The renders outstanding and the regions free for the next, over region
/// type `R`.
pub struct Renders<R> {
    /// Regions free for the next render, each `bytes` long.
    spare: Vec<R>,
    /// The bytes a region holds at the size last asked for.
    bytes: usize,
    pending: Vec<Pending<R>>,
    /// The desktop answered that this window has all the renders pending it
    /// will take, so nothing more is asked until one concludes.
    full: bool,
    /// Moved on whenever the pictures [`asked`](Self::asked) answers for
    /// change, so a question settled against one set is asked again against
    /// the next.
    changes: u64,
}

/// A render asked for and not yet answered, and the region it lands in.
struct Pending<R> {
    wanted: PictureWanted,
    /// Asked before the desktop moved, so its answer is only waited for.
    stale: bool,
    region: R,
}

impl<R> Default for Renders<R> {
    fn default() -> Self {
        Self::new()
    }
}

impl<R> Renders<R> {
    /// Nothing asked for and no region made.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            spare: Vec::new(),
            bytes: 0,
            pending: Vec::new(),
            full: false,
            changes: 0,
        }
    }

    /// Where the pictures [`asked`](Self::asked) answers for stand: equal
    /// for two calls exactly when nothing was asked or answered between them.
    #[must_use]
    pub const fn changes(&self) -> u64 {
        self.changes
    }

    /// Whether another render may be asked for now.
    #[must_use]
    pub const fn may_ask(&self) -> bool {
        !self.full
    }

    /// Whether nothing is pending.
    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.pending.is_empty()
    }

    /// Whether `subject` is already being rendered.
    #[must_use]
    pub fn asked(&self, subject: PreviewSubject) -> bool {
        self.pending
            .iter()
            .any(|pending| pending.wanted.subject == subject)
    }

    /// A region for a picture of `bytes`: a spare one of that size, or one
    /// `create` makes.
    ///
    /// Spare regions of another size are let go first, so re-rendering at a
    /// new scale never holds both sizes at once.
    pub fn region(&mut self, bytes: usize, create: impl FnOnce(usize) -> Option<R>) -> Option<R> {
        if self.bytes != bytes {
            self.spare.clear();
            self.bytes = bytes;
        }
        self.spare.pop().or_else(|| create(bytes))
    }

    /// The desktop accepted a render of `wanted` into `region`.
    pub fn accepted(&mut self, wanted: PictureWanted, region: R) {
        self.pending.push(Pending {
            wanted,
            stale: false,
            region,
        });
        self.changes = self.changes.wrapping_add(1);
    }

    /// `region` was taken for a render that was never asked for, so it is kept
    /// for the next.
    pub fn unused(&mut self, region: R) {
        self.spare.push(region);
    }

    /// The desktop declined a render with `err`, and `region` was never used.
    /// Answers whether the picture itself is refused.
    ///
    /// A window already holding all the renders the desktop runs at once, or
    /// one of this very picture, is waited on rather than refused: the picture
    /// is asked for again once a render concludes. Anything else is a refusal
    /// the picture keeps its placeholder for.
    pub fn declined(&mut self, err: Errno, region: R) -> bool {
        self.unused(region);
        match err {
            Errno::LimitExceeded | Errno::AlreadyExists => {
                self.full = true;
                false
            }
            _ => true,
        }
    }

    /// A render of `subject` at `width`×`height` concluded.
    ///
    /// `land` is handed the picture it answers and its region, unless the
    /// render was asked for before the desktop moved; either way the render
    /// frees its place and its region is kept for the next. Answers `false`
    /// for an answer this window is not waiting on, which lands nothing but
    /// still frees a place: the desktop answers only what it accepted, even
    /// an acceptance a failed call never reported here.
    pub fn concluded(
        &mut self,
        (subject, width, height): (PreviewSubject, u16, u16),
        land: impl FnOnce(PictureWanted, &mut R),
    ) -> bool {
        self.full = false;
        let Some(at) = self.pending.iter().position(|pending| {
            let wanted = pending.wanted;
            (wanted.subject, wanted.width, wanted.height) == (subject, width, height)
        }) else {
            return false;
        };
        let mut pending = self.pending.swap_remove(at);
        self.changes = self.changes.wrapping_add(1);
        if !pending.stale {
            land(pending.wanted, &mut pending.region);
        }
        if pending.wanted.bytes() == self.bytes {
            self.spare.push(pending.region);
        }
        true
    }

    /// The desktop moved: every render outstanding was asked for at a size
    /// that may no longer be drawn, so each answer is waited for and let go.
    pub fn restart(&mut self) {
        for pending in &mut self.pending {
            pending.stale = true;
        }
    }

    /// Let go of every region no render is using.
    pub fn trim(&mut self) {
        self.spare.clear();
    }
}

#[cfg(test)]
#[path = "renders_tests.rs"]
mod tests;
