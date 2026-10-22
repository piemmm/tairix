//! Whether a tree copy would land on or inside the directory it copies, so
//! no copy ever reproduces itself.
//!
//! [`landing`] answers before anything is written, by the path the kernel
//! resolves the target to and the identity of each directory on it, so
//! neither a link, a `..` nor a second spelling of a volume hides the source.
//! [`Descent`] holds the walk to it while the copy runs: a directory renamed
//! into the source meanwhile, or reached again through a link, is refused
//! when the walk meets it.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::{Errno, FileId};
use tairix_path::{parse, Path};

/// What the containment check reads from the filesystem.
pub trait Ancestry {
    /// `path` as the kernel resolves it — each link followed and each `..`
    /// applied to the directories walked — its last component allowed to be
    /// absent.
    ///
    /// # Errors
    ///
    /// The resolution's refusal: [`Errno::NotFound`] for a missing directory
    /// on the way, [`Errno::LinkLoop`] for a cycle of links.
    fn canonical(&self, path: &str) -> Result<String, Errno>;

    /// The identity of the node `path` names, a final link not followed.
    ///
    /// # Errors
    ///
    /// The probe's refusal.
    fn identity(&self, path: &str) -> Result<FileId, Errno>;
}

/// Where a copy's target stands to the directory it copies.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Landing {
    /// Outside it: the copy may go ahead.
    Apart,
    /// The directory itself, by another name or the same.
    Same,
    /// Inside it, at any depth.
    Within,
}

/// Where `target` stands to the directory `source`, whose identity is
/// `source_id` as the copy found it ([`FileId::NONE`] when its volume offers
/// none, so only the resolved paths are compared).
///
/// # Errors
///
/// Either path's resolution failing, an answer the kernel gave that is not a
/// path ([`Errno::OutOfRange`]), or a directory on the target's path that
/// cannot be identified.
pub fn landing(
    fs: &dyn Ancestry,
    source: &str,
    source_id: FileId,
    target: &str,
) -> Result<Landing, Errno> {
    let target = resolved(fs, target)?;
    let source = resolved(fs, source)?;
    if target.root() == source.root() && target.components().starts_with(source.components()) {
        return Ok(if target.components().len() == source.components().len() {
            Landing::Same
        } else {
            Landing::Within
        });
    }
    if source_id.is_none() {
        return Ok(Landing::Apart);
    }
    let depth = target.components().len();
    for len in (0..=depth).rev() {
        let Some(ancestor) = target.prefix(len) else {
            continue;
        };
        let identity = match fs.identity(&ancestor) {
            // The target's own name may be vacant; a vacant name is not the
            // source.
            Err(Errno::NotFound) if len == depth => continue,
            other => other?,
        };
        if identity == source_id {
            return Ok(if len == depth {
                Landing::Same
            } else {
                Landing::Within
            });
        }
    }
    Ok(Landing::Apart)
}

fn resolved(fs: &dyn Ancestry, path: &str) -> Result<Path, Errno> {
    parse(&fs.canonical(path)?).map_err(|_| Errno::OutOfRange)
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

#[cfg(test)]
#[path = "within_tests.rs"]
mod tests;
