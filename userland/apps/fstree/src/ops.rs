//! The file-operation planner and executor over the [`Fs`] seam.
//!
//! Every mutation the panes trigger — copy, move, rename, delete, mkdir —
//! is planned and validated here before any I/O, then driven by the
//! resumable [`FileOp`] executor so a per-file overwrite question can pause
//! the work, hand the decision to the key loop, and continue. A file is
//! copied by the shared copy engine (`lib/fscopy`): its bytes through a
//! bounded buffer with its holes kept, then its extended attributes, those
//! the destination cannot keep gathered for one report. The executor reads
//! directories incrementally as it reaches them and fails closed: the first
//! refused step aborts the operation with the kernel's own [`Errno`], and a
//! partially made file is removed rather than left looking complete.
//!
//! # A symbolic link is a name, not the thing it names
//!
//! Every verb acts on the link: a copy **recreates** it with the same stored
//! target (streaming its bytes would leave a regular file holding the
//! target's text), and a move or a delete acts on the link while what it
//! named survives. A link already sitting at a destination is asked about and
//! then **removed** before the new object is created, because a create or
//! truncate follows a final link and would otherwise act on whatever it
//! pointed at — which is why the destination probe never follows one
//! ([`crate::fs::Fs::stat_kind`]).

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::RefCell;
use core::fmt::Write as _;

use tairix_abi::fs::SeekRegion;
use tairix_abi::{Errno, FileId, FileKind, RenameFlags};
use tairix_fscopy::{
    copy_attributes, landing, Ancestry, AttrError, AttributeStore, Attributes, DataCopy, DataError,
    Descent, Landing, Omissions, Refusal, Source, Sparse, Target, CHUNK_LEN,
};
use tairix_path::{join, leaf_name};

use crate::fs::{Fs, RenameOutcome};

/// Why a planned operation was refused before any I/O, or how it failed
/// mid-flight. Rendered on the message line; nothing was changed unless
/// the variant says otherwise.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OpError {
    /// The typed destination could not be parsed as a path.
    BadPath(String),
    /// Source and destination name the same entry.
    SamePath,
    /// The destination lies inside the source directory's own subtree.
    IntoItself,
    /// The destination exists as a directory where a file must go (or the
    /// reverse); carries the blocking path.
    KindMismatch(String),
    /// A filesystem step was refused; carries the path and the kernel's
    /// [`Errno`]. When mid-copy, the partial target has been removed.
    Fs(String, Errno),
    /// Setting the extended attribute `key` on `path` failed for a reason
    /// that is no attribute's; the partial target has been removed.
    Attribute {
        /// The copy's target.
        path: String,
        /// The attribute refused.
        key: String,
        /// The refusal.
        errno: Errno,
    },
}

impl OpError {
    /// The one-line report the message line shows.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::BadPath(reason) => format!("bad path: {reason}"),
            Self::SamePath => String::from("source and destination are the same entry"),
            Self::IntoItself => String::from("cannot copy or move a directory into itself"),
            Self::KindMismatch(path) => format!("{path}: exists with a different kind"),
            Self::Fs(path, errno) => format!("{path}: {errno:?}"),
            Self::Attribute { path, key, errno } => format!(
                "{path}: attribute {}: {errno:?}",
                tairix_fsmeta::attr::display_value(key.as_bytes())
            ),
        }
    }
}

/// Normalise the destination the user typed, against the directory the
/// file pane lists. A relative spelling is joined onto `base`; an absolute
/// one (view- or alias-rooted) stands alone. The result is the canonical
/// spelling `lib/path` prints — the kernel stays the one authoriser.
///
/// # Errors
///
/// [`OpError::BadPath`] when the spelling fails the shared path grammar.
pub fn resolve_destination(base: &str, typed: &str) -> Result<String, OpError> {
    let parsed = tairix_path::parse(typed).map_err(|e| OpError::BadPath(e.to_string()))?;
    if parsed.is_absolute() {
        return Ok(parsed.to_string());
    }
    let joined = join(base, typed);
    let parsed = tairix_path::parse(&joined).map_err(|e| OpError::BadPath(e.to_string()))?;
    Ok(parsed.to_string())
}

/// Whether `path` lies strictly inside the directory `ancestor` (component
/// boundary respected; equal paths are not "inside").
#[must_use]
pub fn is_inside(ancestor: &str, path: &str) -> bool {
    let ancestor = ancestor.trim_end_matches('/');
    match path.strip_prefix(ancestor) {
        Some(rest) => rest.starts_with('/') && rest.len() > 1,
        None => false,
    }
}

/// The directory `path`'s final component lives in (`/a/b` maps to `/a`;
/// a top-level entry maps to `/`).
#[must_use]
pub fn parent_of(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(0) => "/",
        Some(slash) => &trimmed[..slash],
        None => trimmed,
    }
}

/// Probe `path`, mapping a missing entry to `None` so a destination can be
/// checked for existence without treating absence as failure.
///
/// # Errors
///
/// Any non-[`Errno::NotFound`] refusal, as [`OpError::Fs`].
fn probe(fs: &mut dyn Fs, path: &str) -> Result<Option<FileKind>, OpError> {
    match fs.stat_kind(path) {
        Ok(kind) => Ok(Some(kind)),
        Err(Errno::NotFound) => Ok(None),
        Err(errno) => Err(OpError::Fs(String::from(path), errno)),
    }
}

/// Resolve where a transfer of `src` must land, given the normalised
/// destination the user typed: an existing directory receives the source
/// *inside* it under its base name; anything else is the target itself.
/// Refuses, before anything is written, a target that is the source or lies
/// inside a directory source's own subtree — by the path the kernel resolves
/// it to and the identity of each directory on it, so no link, `..` or alias
/// spelling hides the source.
///
/// # Errors
///
/// [`OpError::SamePath`], [`OpError::IntoItself`], or a probe failure.
pub fn plan_target(
    fs: &mut dyn Fs,
    src: &str,
    src_kind: FileKind,
    dst: &str,
) -> Result<String, OpError> {
    let target = match probe(fs, dst)? {
        Some(FileKind::Directory) => join(dst, leaf_name(src)),
        _ => String::from(dst),
    };
    if target.trim_end_matches('/') == src.trim_end_matches('/') {
        return Err(OpError::SamePath);
    }
    if src_kind == FileKind::Directory {
        let id = fs
            .identity(src)
            .map_err(|errno| OpError::Fs(String::from(src), errno))?;
        let shared = Shared(RefCell::new(fs));
        match landing(&shared, src, id, &target)
            .map_err(|errno| OpError::Fs(target.clone(), errno))?
        {
            Landing::Apart => {}
            Landing::Same => return Err(OpError::SamePath),
            Landing::Within => return Err(OpError::IntoItself),
        }
    }
    Ok(target)
}

/// Whether a paused [`FileOp`] transfers or deletes, for the prompts and
/// the completion report.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum OpKind {
    /// Reproduce the source at the target, leaving the source in place.
    Copy,
    /// Reproduce the source at the target, then remove the source (the
    /// cross-volume fallback; a same-volume move renames instead).
    Move,
    /// Remove the source and, for a directory, everything under it.
    Delete,
}

/// A target name that is already taken, awaiting the user's per-entry
/// decision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Conflict {
    /// The source entry whose transfer is paused.
    pub src: String,
    /// The existing name the transfer would replace.
    pub dst: String,
    /// What that name currently holds, so the question names it: replacing a
    /// symbolic link is a different thing from overwriting a file's bytes,
    /// and the user is told which they are agreeing to.
    pub occupant: FileKind,
}

/// The user's answer to a [`Conflict`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Decision {
    /// Overwrite the existing target and continue.
    Overwrite,
    /// Leave the existing target (and, on a move, the source) and continue.
    Skip,
    /// Stop the operation; steps already applied stay applied.
    Cancel,
}

/// What one [`FileOp::advance`] call achieved.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OpProgress {
    /// Every step completed; the operation is finished.
    Done,
    /// A [`Conflict`] awaits a [`Decision`] through [`FileOp::resolve`].
    NeedsDecision,
    /// A step failed; the operation stopped with this error.
    Failed(OpError),
}

/// One queued unit of work, processed last-in-first-out so a directory's
/// children complete before the step queued beneath them.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Work {
    /// Rename one entry atomically, falling back to a transfer when the
    /// volumes differ. `approved` carries what the user already agreed to
    /// replace — the destination is probed *before* the rename, so an
    /// existing name is asked about, never silently replaced.
    Rename {
        src: String,
        kind: FileKind,
        dst: String,
        approved: Option<FileKind>,
    },
    /// Copy one entry to `dst` (recursing into a directory by pushing its
    /// children, recreating a symbolic link); on a move, a copied entry's
    /// source is removed in the same step. `approved` carries what the user
    /// already agreed to replace.
    Transfer {
        src: String,
        kind: FileKind,
        dst: String,
        approved: Option<FileKind>,
    },
    /// Remove one entry (expanding a directory into child removals plus a
    /// directory removal beneath them).
    Remove { path: String, kind: FileKind },
    /// Remove one directory the steps queued above it emptied.
    RemoveEmptiedDir { path: String },
    /// Leave the directory a transfer entered, its entries all queued above.
    Leave,
}

/// A resumable file operation: a work stack advanced until it finishes,
/// fails, or pauses on a [`Conflict`]. Directories are read incrementally
/// as the walk reaches them (memory follows the frontier, never the
/// subtree), and a paused operation holds no filesystem handle — only the
/// paths still to process.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileOp {
    kind: OpKind,
    stack: Vec<Work>,
    /// The paused question and the pre-approved work item an
    /// [`Decision::Overwrite`] pushes back.
    conflict: Option<(Conflict, Work)>,
    /// Sources the user skipped; an emptied-source-dir removal above a
    /// skipped file is silently withheld so a skip never fails the move.
    skipped_srcs: Vec<String>,
    /// Entries completed (files copied/renamed/removed, directories
    /// removed), for the completion report.
    done: usize,
    /// Conflicts the user skipped, for the completion report.
    skipped: usize,
    /// What the copies' destinations could not keep, for the completion
    /// report.
    omissions: Omissions,
    /// The directories the walk is inside, so it never enters one again.
    descent: Descent,
    /// Whether the user cancelled the remaining steps.
    cancelled: bool,
}

impl FileOp {
    /// A copy of `src` (of `kind`) to the planned `target`.
    #[must_use]
    pub fn copy(src: &str, kind: FileKind, target: &str) -> Self {
        Self::new(
            OpKind::Copy,
            Work::Transfer {
                src: String::from(src),
                kind,
                dst: String::from(target),
                approved: None,
            },
        )
    }

    /// A move of `src` (of `kind`) to the planned `target`: an atomic
    /// rename where the volume allows it, the copy-then-remove fallback
    /// where it does not.
    #[must_use]
    pub fn move_to(src: &str, kind: FileKind, target: &str) -> Self {
        Self::new(
            OpKind::Move,
            Work::Rename {
                src: String::from(src),
                kind,
                dst: String::from(target),
                approved: None,
            },
        )
    }

    /// A delete of `path` (of `kind`); a directory is removed with
    /// everything under it, children before parents.
    #[must_use]
    pub fn delete(path: &str, kind: FileKind) -> Self {
        Self::new(
            OpKind::Delete,
            Work::Remove {
                path: String::from(path),
                kind,
            },
        )
    }

    fn new(kind: OpKind, first: Work) -> Self {
        Self {
            kind,
            stack: alloc::vec![first],
            conflict: None,
            skipped_srcs: Vec::new(),
            done: 0,
            skipped: 0,
            omissions: Omissions::default(),
            descent: Descent::default(),
            cancelled: false,
        }
    }

    /// The operation's kind, for the prompts and reports.
    #[must_use]
    pub fn kind(&self) -> OpKind {
        self.kind
    }

    /// The paused overwrite question, when [`FileOp::advance`] reported
    /// [`OpProgress::NeedsDecision`].
    #[must_use]
    pub fn conflict(&self) -> Option<&Conflict> {
        self.conflict.as_ref().map(|(conflict, _)| conflict)
    }

    /// Whether the user cancelled the operation's remaining steps at an
    /// overwrite question; work already applied stayed applied. The batch
    /// driver reads this to tell a cancelled entry from a completed one.
    #[must_use]
    pub fn cancelled(&self) -> bool {
        self.cancelled
    }

    /// The extended attributes the copies so far could not carry, and why.
    #[must_use]
    pub fn omissions(&self) -> &Omissions {
        &self.omissions
    }

    /// Answer the paused question: overwrite re-queues the approved step,
    /// skip withholds it (and any removal of its emptied ancestors), and
    /// cancel drops every remaining step — work already applied stays.
    pub fn resolve(&mut self, decision: Decision) {
        let Some((conflict, approved)) = self.conflict.take() else {
            return;
        };
        match decision {
            Decision::Overwrite => self.stack.push(approved),
            Decision::Skip => {
                self.skipped += 1;
                self.skipped_srcs.push(conflict.src);
            }
            Decision::Cancel => {
                self.cancelled = true;
                self.stack.clear();
            }
        }
    }

    /// Drive the work stack until it empties, pauses on a conflict, or
    /// fails. Call again after [`FileOp::resolve`].
    pub fn advance(&mut self, fs: &mut dyn Fs) -> OpProgress {
        if self.conflict.is_some() {
            return OpProgress::NeedsDecision;
        }
        while let Some(work) = self.stack.pop() {
            let step = match work {
                Work::Rename {
                    src,
                    kind,
                    dst,
                    approved,
                } => self.step_rename(fs, src, kind, dst, approved),
                Work::Transfer {
                    src,
                    kind,
                    dst,
                    approved,
                } => self.step_transfer(fs, src, kind, dst, approved),
                Work::Remove { path, kind } => self.step_remove(fs, &path, kind),
                Work::RemoveEmptiedDir { path } => self.step_remove_emptied(fs, &path),
                Work::Leave => {
                    self.descent.leave();
                    Ok(())
                }
            };
            match step {
                Ok(()) => {}
                Err(StepEnd::Paused) => return OpProgress::NeedsDecision,
                Err(StepEnd::Failed(error)) => return OpProgress::Failed(error),
            }
        }
        OpProgress::Done
    }

    /// The one-line completion report for the message line.
    #[must_use]
    pub fn summary(&self) -> String {
        let verb = match self.kind {
            OpKind::Copy => "copied",
            OpKind::Move => "moved",
            OpKind::Delete => "deleted",
        };
        let noun = if self.done == 1 { "entry" } else { "entries" };
        let mut text = format!("{verb} {} {noun}", self.done);
        if self.skipped > 0 {
            // Writing to a `String` cannot fail.
            let _ = write!(text, ", {} skipped", self.skipped);
        }
        if self.cancelled {
            text.push_str(" (cancelled)");
        }
        text
    }

    fn step_rename(
        &mut self,
        fs: &mut dyn Fs,
        src: String,
        kind: FileKind,
        dst: String,
        approved: Option<FileKind>,
    ) -> Result<(), StepEnd> {
        // Until the user has agreed to replace, the kernel is asked to refuse
        // an occupied destination rather than replace it, so a name created
        // after any look is never destroyed unasked — and the source's own
        // entry, re-spelled on a volume that ignores case, is no occupant.
        let flags = if approved.is_some() {
            RenameFlags::empty()
        } else {
            RenameFlags::NO_REPLACE
        };
        let outcome = match fs.rename(&src, &dst, flags) {
            Err(Errno::AlreadyExists) if approved.is_none() => {
                // Any occupied leaf name is asked about — a link included,
                // because replacing one is a real loss the user must agree
                // to. A directory target was already redirected into by the
                // planner; whatever remains (dir over file, dir over dir) is
                // the kernel's to judge, and its refusal surfaces unchanged.
                let occupant = probe(fs, &dst).map_err(StepEnd::Failed)?;
                if let Some(occupant) = occupant.filter(|held| replaces_leaf(kind, *held)) {
                    return Err(self.pause(
                        src.clone(),
                        dst.clone(),
                        occupant,
                        Work::Rename {
                            src,
                            kind,
                            dst,
                            approved: Some(occupant),
                        },
                    ));
                }
                fs.rename(&src, &dst, RenameFlags::empty())
            }
            other => other,
        };
        match outcome {
            Ok(RenameOutcome::Renamed) => {
                self.done += 1;
                Ok(())
            }
            Ok(RenameOutcome::CrossDevice) => {
                self.stack.push(Work::Transfer {
                    src,
                    kind,
                    dst,
                    approved,
                });
                Ok(())
            }
            Err(errno) => Err(StepEnd::Failed(OpError::Fs(src, errno))),
        }
    }

    fn step_transfer(
        &mut self,
        fs: &mut dyn Fs,
        src: String,
        kind: FileKind,
        dst: String,
        approved: Option<FileKind>,
    ) -> Result<(), StepEnd> {
        match kind {
            // A leaf — a file's bytes or a link's stored target — replaces an
            // occupied name only with the user's agreement, and replacing it
            // *removes* that name first so nothing is ever written through a
            // link that was already sitting there.
            FileKind::Regular | FileKind::Symlink => {
                let approved = if approved.is_some() {
                    approved
                } else {
                    match probe(fs, &dst).map_err(StepEnd::Failed)? {
                        None => None,
                        Some(FileKind::Directory) => {
                            return Err(StepEnd::Failed(OpError::KindMismatch(dst)));
                        }
                        Some(held) => {
                            return Err(self.pause(
                                src.clone(),
                                dst.clone(),
                                held,
                                Work::Transfer {
                                    src,
                                    kind,
                                    dst,
                                    approved: Some(held),
                                },
                            ));
                        }
                    }
                };
                // An approved occupant goes once the source has answered, before
                // the new object is made: a create follows a final link, a new
                // link never replaces a name, and a file emptied in place would
                // be the source itself were the two names one node.
                let replacing = approved.is_some();
                match kind {
                    // A link is *recreated* with the target it stores. Copying
                    // its bytes would leave a regular file holding the
                    // target's text, and following it would copy something the
                    // link only points at.
                    FileKind::Symlink => {
                        let target = fs
                            .read_link(&src)
                            .map_err(|errno| OpError::Fs(String::from(&src), errno))
                            .map_err(StepEnd::Failed)?;
                        if replacing {
                            fs.remove_file(&dst).map_err(|errno| {
                                StepEnd::Failed(OpError::Fs(dst.clone(), errno))
                            })?;
                        }
                        fs.create_link(&target, &dst)
                            .map_err(|errno| StepEnd::Failed(OpError::Fs(dst.clone(), errno)))?;
                    }
                    FileKind::Regular | FileKind::Directory => {
                        let left = copy_file(fs, &src, &dst, replacing).map_err(StepEnd::Failed)?;
                        self.omissions.note(&left);
                    }
                }
                if self.kind == OpKind::Move {
                    // The name as typed: removing a link removes the link,
                    // never what it names.
                    fs.remove_file(&src)
                        .map_err(|errno| StepEnd::Failed(OpError::Fs(src.clone(), errno)))?;
                }
                self.done += 1;
                Ok(())
            }
            FileKind::Directory => {
                let fail_on = |path: &str| {
                    let path = String::from(path);
                    move |errno| StepEnd::Failed(OpError::Fs(path, errno))
                };
                // A directory renamed into the source meanwhile is met here,
                // and refused rather than copied into its own copy.
                let source_id = fs.identity(&src).map_err(fail_on(&src))?;
                if self.descent.refuses(source_id) {
                    return Err(StepEnd::Failed(OpError::IntoItself));
                }
                match probe(fs, &dst).map_err(StepEnd::Failed)? {
                    // An existing directory target is merged into.
                    Some(FileKind::Directory) => {}
                    // A leaf cannot hold a directory's contents, and a link
                    // is a leaf however it resolves.
                    Some(FileKind::Regular | FileKind::Symlink) => {
                        return Err(StepEnd::Failed(OpError::KindMismatch(dst)));
                    }
                    None => {
                        fs.mkdir(&dst).map_err(fail_on(&dst))?;
                        let left = carry_attributes(&Shared(RefCell::new(&mut *fs)), &src, &dst)
                            .map_err(StepEnd::Failed)?;
                        self.omissions.note(&left);
                    }
                }
                let made_id = fs.identity(&dst).map_err(fail_on(&dst))?;
                let entries = fs.list_dir(&src).map_err(fail_on(&src))?;
                self.descent
                    .enter(source_id, made_id)
                    .map_err(fail_on(&dst))?;
                self.stack.push(Work::Leave);
                if self.kind == OpKind::Move {
                    // Queued beneath the children, so the source directory
                    // is removed only once they have all moved out.
                    self.stack
                        .push(Work::RemoveEmptiedDir { path: src.clone() });
                }
                for entry in entries {
                    self.stack.push(Work::Transfer {
                        src: join(&src, &entry.name),
                        kind: entry.kind,
                        dst: join(&dst, &entry.name),
                        approved: None,
                    });
                }
                Ok(())
            }
        }
    }

    fn step_remove(&mut self, fs: &mut dyn Fs, path: &str, kind: FileKind) -> Result<(), StepEnd> {
        match kind {
            // A link is removed as the leaf it is: `fs_unlink` keeps the name
            // as typed, so what the link points at is untouched.
            FileKind::Symlink | FileKind::Regular => {
                fs.remove_file(path)
                    .map_err(|errno| StepEnd::Failed(OpError::Fs(String::from(path), errno)))?;
                self.done += 1;
                Ok(())
            }
            FileKind::Directory => {
                let entries = fs
                    .list_dir(path)
                    .map_err(|errno| StepEnd::Failed(OpError::Fs(String::from(path), errno)))?;
                self.stack.push(Work::RemoveEmptiedDir {
                    path: String::from(path),
                });
                for entry in entries {
                    self.stack.push(Work::Remove {
                        path: join(path, &entry.name),
                        kind: entry.kind,
                    });
                }
                Ok(())
            }
        }
    }

    fn step_remove_emptied(&mut self, fs: &mut dyn Fs, path: &str) -> Result<(), StepEnd> {
        // A skip left a source in place on purpose; its ancestors are then
        // intentionally not emptied, so their removal is withheld.
        if self
            .skipped_srcs
            .iter()
            .any(|skipped| is_inside(path, skipped))
        {
            return Ok(());
        }
        fs.remove_dir(path)
            .map_err(|errno| StepEnd::Failed(OpError::Fs(String::from(path), errno)))?;
        self.done += 1;
        Ok(())
    }

    fn pause(&mut self, src: String, dst: String, occupant: FileKind, approved: Work) -> StepEnd {
        self.conflict = Some((Conflict { src, dst, occupant }, approved));
        StepEnd::Paused
    }
}

/// Whether moving a `src` of this kind onto a name already holding
/// `occupant` replaces a *leaf* — the case the user is asked about.
///
/// Two leaves (a file or a link either side) replace one another and lose
/// what was there, so the question is real. A directory on either side is
/// not this engine's to judge: the planner already redirected into an
/// existing directory, and anything left is the kernel's refusal to
/// surface.
const fn replaces_leaf(src: FileKind, occupant: FileKind) -> bool {
    is_leaf(src) && is_leaf(occupant)
}

/// Whether a node of this kind is a leaf — something a transfer replaces
/// whole rather than merges into.
const fn is_leaf(kind: FileKind) -> bool {
    matches!(kind, FileKind::Regular | FileKind::Symlink)
}

/// Why a step ended without completing.
enum StepEnd {
    /// The step paused on a conflict already recorded on the operation.
    Paused,
    /// The step failed; the operation stops with this error.
    Failed(OpError),
}

/// Copy the regular file `src` to `dst` — its bytes with its holes, then the
/// extended attributes `dst`'s volume keeps — answering those it could not
/// carry, first removing the occupant of `dst` the user agreed to replace
/// when `replacing`. A failure part-way removes the partial target
/// (best-effort) so a half-made file never masquerades as the source's copy.
fn copy_file(
    fs: &mut dyn Fs,
    src: &str,
    dst: &str,
    replacing: bool,
) -> Result<Vec<(String, Refusal)>, OpError> {
    let len = fs
        .size(src)
        .map_err(|errno| OpError::Fs(String::from(src), errno))?;
    // Sized to the file, so a tree of small files is no run of chunk-sized
    // allocations.
    let want = usize::try_from(len).map_or(CHUNK_LEN, |len| len.clamp(1, CHUNK_LEN));
    let mut buf = Vec::new();
    if !tairix_util::fallible::grow_to(&mut buf, want, 0) {
        return Err(OpError::Fs(String::from(src), Errno::OutOfMemory));
    }
    if replacing {
        fs.remove_file(dst)
            .map_err(|errno| OpError::Fs(String::from(dst), errno))?;
    }
    fs.create(dst)
        .map_err(|errno| OpError::Fs(String::from(dst), errno))?;
    let copied = {
        let shared = Shared(RefCell::new(&mut *fs));
        DataCopy::new(len, Sparse::Auto)
            .run(&mut shared.file(src), &mut shared.file(dst), &mut buf)
            .map_err(|failure| match failure {
                DataError::Read(errno) => OpError::Fs(String::from(src), errno),
                DataError::Write(errno) => OpError::Fs(String::from(dst), errno),
            })
            .and_then(|()| carry_attributes(&shared, src, dst))
    };
    if copied.is_err() {
        let _ = fs.remove_file(dst);
    }
    copied
}

/// Carry `src`'s extended attributes to the new `dst`, answering those its
/// volume could not keep.
fn carry_attributes(
    store: &Shared<'_, '_>,
    src: &str,
    dst: &str,
) -> Result<Vec<(String, Refusal)>, OpError> {
    copy_attributes(store, src, dst, Attributes::Carry).map_err(|err| match err {
        AttrError::Read(errno) => OpError::Fs(String::from(src), errno),
        AttrError::Write { key, error } => OpError::Attribute {
            path: String::from(dst),
            key,
            errno: error,
        },
    })
}

/// The filesystem a copy's two ends and its attributes share, each call
/// borrowing it for that call alone.
struct Shared<'a, 'f>(RefCell<&'a mut (dyn Fs + 'f)>);

impl Ancestry for Shared<'_, '_> {
    fn canonical(&self, path: &str) -> Result<String, Errno> {
        self.0.borrow_mut().canonical(path)
    }

    fn identity(&self, path: &str) -> Result<FileId, Errno> {
        self.0.borrow_mut().identity(path)
    }
}

impl<'a, 'f> Shared<'a, 'f> {
    fn file<'s>(&'s self, path: &'s str) -> SharedFile<'s, 'a, 'f> {
        SharedFile { fs: self, path }
    }
}

/// One file of a copy, read or written through the [`Shared`] filesystem.
struct SharedFile<'s, 'a, 'f> {
    fs: &'s Shared<'a, 'f>,
    path: &'s str,
}

impl Source for SharedFile<'_, '_, '_> {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, Errno> {
        self.fs.0.borrow_mut().read(self.path, offset, buf)
    }

    fn next_region(&mut self, offset: u64, region: SeekRegion) -> Result<Option<u64>, Errno> {
        self.fs
            .0
            .borrow_mut()
            .next_region(self.path, offset, region)
    }

    fn size(&mut self) -> Result<u64, Errno> {
        self.fs.0.borrow_mut().size(self.path)
    }
}

impl Target for SharedFile<'_, '_, '_> {
    fn write_all_at(&mut self, offset: u64, bytes: &[u8]) -> Result<(), Errno> {
        self.fs.0.borrow_mut().write(self.path, offset, bytes)
    }

    fn set_len(&mut self, len: u64) -> Result<(), Errno> {
        self.fs.0.borrow_mut().set_len(self.path, len)
    }
}

impl AttributeStore for Shared<'_, '_> {
    fn attr_keys(&self, path: &str) -> Result<Vec<String>, Errno> {
        self.0.borrow_mut().attr_list(path)
    }

    fn attr_value(&self, path: &str, key: &str) -> Result<Vec<u8>, Errno> {
        self.0.borrow_mut().attr_get(path, key)
    }

    fn attr_set(&self, path: &str, key: &str, value: &[u8]) -> Result<(), Errno> {
        self.0.borrow_mut().attr_set(path, key, value)
    }
}
