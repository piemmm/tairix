//! Whether a tree copy would land on or inside the directory it copies, so
//! no copy ever reproduces itself.
//!
//! [`landing`] answers before anything is written, from the directories the
//! copy holds: it climbs from where the copy would write through `..`,
//! comparing each directory's identity with the source's, so neither a link,
//! a `..` nor a second spelling of a volume hides the source. [`Descent`]
//! holds the walk to it while the copy runs: a directory renamed into the
//! source meanwhile, or reached again through a link, is refused when the
//! walk meets it.

use alloc::vec::Vec;

use tairix_abi::{Errno, FileId};

use crate::tree::Dir;

/// Directories a climb passes before it gives up: past this depth a
/// directory has no path the kernel would take.
const CLIMB_MAX: usize = tairix_abi::FS_PATH_MAX / 2;

/// Where a directory stands to the directory a copy copies.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Landing {
    /// Outside it: the copy may go ahead.
    Apart,
    /// The directory itself, by another name or the same.
    Same,
    /// Inside it, at any depth.
    Within,
}

/// Which of a [`landing`] check's two directories failed it, and how.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Unresolved {
    /// The directory being copied.
    Source(Errno),
    /// The directory the copy would write in, or one above it.
    Target(Errno),
}

impl Unresolved {
    /// Of `source` and `target`, the path that failed, and its refusal.
    #[must_use]
    pub fn at<'a>(self, source: &'a str, target: &'a str) -> (&'a str, Errno) {
        match self {
            Self::Source(errno) => (source, errno),
            Self::Target(errno) => (target, errno),
        }
    }
}

/// Where the held directory `dir` stands to the held directory `source`:
/// the same directory, beneath it at any depth, or apart. A source whose
/// volume offers no identity ([`FileId::NONE`]) is apart from everything.
///
/// # Errors
///
/// [`Unresolved::Source`] when the source cannot be identified;
/// [`Unresolved::Target`] when `dir` or a directory above it cannot be, or
/// opened, or the climb passes a depth no path reaches
/// ([`Errno::LengthOutOfRange`]).
pub fn landing<D: Dir>(source: &D, dir: &D) -> Result<Landing, Unresolved> {
    let wanted = source.stat().map_err(Unresolved::Source)?.id;
    if wanted.is_none() {
        return Ok(Landing::Apart);
    }
    let mut above: Option<D> = None;
    let mut below = FileId::NONE;
    for depth in 0..CLIMB_MAX {
        let at = above.as_ref().unwrap_or(dir);
        let id = at.stat().map_err(Unresolved::Target)?.id;
        if id == wanted {
            return Ok(if depth == 0 {
                Landing::Same
            } else {
                Landing::Within
            });
        }
        // The root is its own `..`; one with no identity shows as two in a
        // row.
        if depth > 0 && id == below && (!id.is_none() || depth > 1) {
            return Ok(Landing::Apart);
        }
        below = id;
        above = Some(at.open_dir("..").map_err(Unresolved::Target)?);
    }
    Err(Unresolved::Target(Errno::LengthOutOfRange))
}

/// Whether the directory `name` of `dir` holds `into`, at any depth: why a
/// rename of it to `into` would be refused.
pub fn lands_inside<D: Dir>(dir: &D, name: &str, into: &D) -> bool {
    dir.open_dir(name)
        .ok()
        .and_then(|held| landing(&held, into).ok())
        .is_some_and(|landed| landed != Landing::Apart)
}

/// The directories a tree copy is inside, so its walk enters none of them
/// again: neither a source directory it is already copying, met again
/// through a link, nor a directory the copy made itself.
///
/// It holds two identities a level, so it costs the walk's depth, never the
/// tree's size.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Descent {
    entered: Vec<FileId>,
}

impl Descent {
    /// Whether the walk must refuse to enter the source directory `id`.
    /// A directory its volume cannot identify is never refused.
    #[must_use]
    pub fn refuses(&self, id: FileId) -> bool {
        !id.is_none() && self.entered.contains(&id)
    }

    /// Enter the source directory `source`, being copied to `made`.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfMemory`] when the heap cannot hold another level.
    pub fn enter(&mut self, source: FileId, made: FileId) -> Result<(), Errno> {
        self.entered
            .try_reserve(2)
            .map_err(|_| Errno::OutOfMemory)?;
        self.entered.push(source);
        self.entered.push(made);
        Ok(())
    }

    /// Leave the level [`enter`](Self::enter) last entered.
    pub fn leave(&mut self) {
        let len = self.entered.len().saturating_sub(2);
        self.entered.truncate(len);
    }
}

#[cfg(all(test, feature = "test-support"))]
#[path = "within_tests.rs"]
mod tests;
