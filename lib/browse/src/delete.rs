//! The **delete** model (`plans/NEW-FILEMANAGER.md` `FM7b`): the pure,
//! host-provable core of removing the selected entries.
//!
//! Where the [`clipboard`](crate::clipboard) captures the selection for a move
//! or copy, a [`DeletePlan`] captures it for removal: each marked entry as
//! [`Listed`] and whether it is directory-backed on disk, in listing order. It is the one model behind the Delete verb — the engine names *what*
//! would be removed and *how many* items (so the app can confirm honestly, a
//! `lib/controls` `Dialog` with truthful action warmth); the app removes each
//! target, and everything beneath a directory-backed one, as an operation of
//! the shared engine (`lib/fscopy`) under the user's own identity. Composing
//! this model grants nothing, so the read-only picker never builds a delete
//! plan.
//!
//! Paths are root-first component lists — the same
//! [`folder`](crate::Browser::folder) vocabulary the whole engine uses — and
//! each target carries the node the listing showed, so a removal finding
//! another under the name leaves it alone.
//!
//! [`DeletePlan::new`] is **fail closed**: a plan is built only from a
//! non-empty set of real entries; an empty selection, or any target that names
//! the filesystem root (an empty component list), yields `None` rather than a
//! plan that could remove nothing or the root.
//!

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::fs::FileId;

use crate::entry::Listed;

/// One entry queued for removal, and whether it was listed as
/// directory-backed on disk.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeleteTarget {
    listed: Listed,
    is_directory: bool,
}

impl DeleteTarget {
    /// `listed`, listed as directory-backed when `is_directory`.
    #[must_use]
    pub const fn new(listed: Listed, is_directory: bool) -> Self {
        Self {
            listed,
            is_directory,
        }
    }

    /// The target's root-first component path (never empty in a built
    /// [`DeletePlan`]).
    #[must_use]
    pub fn path(&self) -> &[String] {
        &self.listed.path
    }

    /// The node the target was listed as ([`Listed::id`]).
    #[must_use]
    pub const fn id(&self) -> FileId {
        self.listed.id
    }

    /// `true` when the target was listed as directory-backed — a directory or
    /// a bundle — so a confirmation can say it goes with everything in it.
    #[must_use]
    pub const fn is_directory(&self) -> bool {
        self.is_directory
    }

    /// The target's leaf name — its last path component.
    ///
    /// A [`DeletePlan`] never holds a root (empty) target, so this is always
    /// present.
    #[must_use]
    pub fn name(&self) -> &str {
        self.listed.path.last().map_or("", String::as_str)
    }
}

/// The resolved plan for deleting a selection: one [`DeleteTarget`] per marked
/// entry, in listing order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeletePlan {
    targets: Vec<DeleteTarget>,
}

impl DeletePlan {
    /// Build a plan over `targets`, or `None` when there is nothing to delete
    /// — an empty selection, or any target that names the root (an empty
    /// component list).
    ///
    /// Refusing an empty or root-naming plan here means the Delete verb is
    /// simply unavailable rather than a silent no-op or a catastrophic
    /// whole-root removal.
    #[must_use]
    pub fn new(targets: Vec<DeleteTarget>) -> Option<Self> {
        if targets.is_empty() || targets.iter().any(|target| target.path().is_empty()) {
            return None;
        }
        Some(Self { targets })
    }

    /// The queued targets, one per selected entry, in listing order.
    #[must_use]
    pub fn targets(&self) -> &[DeleteTarget] {
        &self.targets
    }

    /// The number of entries the plan would remove — the count an honest
    /// confirmation reports.
    #[must_use]
    pub fn len(&self) -> usize {
        self.targets.len()
    }

    /// `true` when the plan holds no targets. A built [`DeletePlan`] is never
    /// empty (see [`new`](Self::new)); this exists for callers holding an
    /// already-unwrapped plan.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.targets.is_empty()
    }

    /// `true` when any target is directory-backed, so a confirmation can warn
    /// that folders (and their contents) will be removed recursively rather
    /// than treating every deletion as a single file.
    #[must_use]
    pub fn has_directories(&self) -> bool {
        self.targets.iter().any(DeleteTarget::is_directory)
    }
}
