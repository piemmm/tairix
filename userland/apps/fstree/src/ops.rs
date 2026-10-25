//! The file-operation planner over the [`Fs`] seam, and the operations it
//! starts.
//!
//! Every mutation the panes trigger — copy, move, rename, delete — is planned
//! and validated here before any I/O, then carried out by the shared engine
//! (`tairix_fscopy::op`), every entry reached by name from a directory it
//! holds, never by a joined path. A [`FileOp`] holds the engine's operation
//! so a per-file overwrite question can pause it, hand the decision to the
//! key loop, and continue. What a copy makes takes its source's mode as `cp`
//! gives it, a move keeps its source's mode and owner, and a move across
//! volumes removes, once its copy is whole and on the medium, only what it
//! copied while unchanged: an entry changed meanwhile is kept and said.
//!
//! # A symbolic link is a name, not the thing it names
//!
//! Every verb acts on the link: a copy recreates it with the same stored
//! target, and a move or a delete acts on the link while what it named
//! survives. A link already at a destination is asked about like any file
//! and replaced as itself, only while it is the link asked about, once what
//! replaces it is whole.
//!
//! An operation started on a listed entry carries the node the listing
//! showed, and refuses a name that has come to hold another.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::{self, Write as _};

use tairix_abi::{Errno, FileId, FileKind};
use tairix_fscopy::op::{self, Named, Operation, Progress, Report, Stager, Taken, Verb};
use tairix_fscopy::tree::{Dir, Roots};
use tairix_fscopy::{Landing, Omissions};
use tairix_path::{dir_name, join, leaf_name};
use tairix_util::quote::quote_subject;

pub use tairix_fscopy::op::{Conflict, Decision};

use crate::fs::Fs;

/// Why a planned operation was refused before any I/O, or how it failed
/// mid-flight. Rendered on the message line; nothing was changed unless the
/// variant says otherwise.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OpError {
    /// The typed destination could not be parsed as a path.
    BadPath(String),
    /// Source and destination name the same entry.
    SamePath,
    /// The engine's refusal, or the planner's of the same kind before any
    /// I/O; a failure part-way removes the file it was writing.
    Engine(op::OpError),
}

impl OpError {
    /// The one-line report the message line shows.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::BadPath(reason) => format!("bad path: {reason}"),
            Self::SamePath => String::from("source and destination are the same entry"),
            Self::Engine(op::OpError::IntoItself) => {
                String::from("cannot copy or move a directory into itself")
            }
            Self::Engine(op::OpError::KindMismatch(path)) => {
                format!("{}: exists with a different kind", quote_subject(path))
            }
            Self::Engine(
                op::OpError::Read(path, errno)
                | op::OpError::Write(path, errno)
                | op::OpError::Rename(path, errno)
                | op::OpError::Remove(path, errno),
            ) => refused(path, *errno),
            Self::Engine(op::OpError::Attribute { path, key, errno }) => format!(
                "{}: attribute {}: {errno}",
                quote_subject(path),
                tairix_fsmeta::attr::display_value(key.as_bytes())
            ),
            Self::Engine(op::OpError::Changed(path)) => format!(
                "{}: changed meanwhile, so it was left as it is",
                quote_subject(path)
            ),
        }
    }
}

/// `path: reason`, or the reason alone for a refusal spelled with no path.
fn refused(path: &str, errno: Errno) -> String {
    if path.is_empty() {
        errno.to_string()
    } else {
        format!("{}: {errno}", quote_subject(path))
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

/// Probe `path`, mapping a missing entry to `None` so a destination can be
/// checked for existence without treating absence as failure.
///
/// # Errors
///
/// Any non-[`Errno::NotFound`] refusal.
fn probe(fs: &mut dyn Fs, path: &str) -> Result<Option<FileKind>, OpError> {
    match fs.stat_kind(path) {
        Ok(kind) => Ok(Some(kind)),
        Err(Errno::NotFound) => Ok(None),
        Err(errno) => Err(OpError::Engine(op::OpError::Read(
            String::from(path),
            errno,
        ))),
    }
}

/// Resolve where a transfer of `src` must land, given the normalised
/// destination the user typed: an existing directory receives the source
/// *inside* it under its base name; anything else is the target itself.
/// Refuses, before anything is written, a target that is the source or lies
/// inside a directory source's own subtree — judged from the directories
/// themselves by the shared containment check, so no link, `..` or alias
/// spelling hides the source.
///
/// # Errors
///
/// [`OpError::SamePath`], an into-itself refusal, or a probe failure.
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
        // What the copy writes in: the target merged into, else the
        // directory a new one is made in.
        let merging = probe(fs, &target)? == Some(FileKind::Directory);
        let into = if merging {
            target.as_str()
        } else {
            dir_name(&target)
        };
        let landed = fs.landing(src, into).map_err(|unresolved| {
            let (path, errno) = unresolved.at(src, into);
            OpError::Engine(op::OpError::Read(String::from(path), errno))
        })?;
        match landed {
            Landing::Apart => {}
            Landing::Same if merging => return Err(OpError::SamePath),
            Landing::Same | Landing::Within => {
                return Err(OpError::Engine(op::OpError::IntoItself))
            }
        }
    }
    Ok(target)
}

/// The copy or move of `src`, a path and the node it was listed as, to
/// `target`: each path's directory opened through `roots`, every entry
/// beneath reached by name from one held, a replacement built under a name
/// `stage` gives.
///
/// # Errors
///
/// The refusal opening either directory, naming its path.
pub fn transfer<R: Roots>(
    roots: &R,
    verb: Verb,
    (src, target): ((&str, FileId), &str),
    stage: Stager,
) -> Result<Operation<R::Dir>, op::OpError> {
    Ok(Operation::transfer(
        verb,
        named(roots, src)?,
        named(roots, (target, FileId::NONE))?,
        Taken::Ask,
        stage,
    ))
}

/// The removal of `path`, a path and the node it was listed as, and of
/// everything beneath a directory, its directory opened through `roots`.
///
/// # Errors
///
/// The refusal opening its directory.
pub fn remove<R: Roots>(roots: &R, path: (&str, FileId)) -> Result<Operation<R::Dir>, op::OpError> {
    Ok(Operation::remove(named(roots, path)?))
}

fn named<R: Roots>(roots: &R, (path, seen): (&str, FileId)) -> Result<Named<R::Dir>, op::OpError> {
    let named =
        Named::parent(roots, path).map_err(|errno| op::OpError::Read(String::from(path), errno))?;
    Ok(Named { seen, ..named })
}

/// The engine's operation over whatever directories the [`Fs`] seam holds:
/// what a [`FileOp`] keeps across a question.
pub trait Run {
    /// Carry out at most `units` units ([`Operation::advance`]).
    fn advance(&mut self, units: usize, buf: &mut Vec<u8>) -> Progress;

    /// The question it waits on ([`Operation::conflict`]).
    fn conflict(&self) -> Option<&Conflict>;

    /// Answer the question, or cancel ([`Operation::resolve`]).
    fn resolve(&mut self, decision: Decision);

    /// What it has done ([`Operation::report`]).
    fn report(&self) -> &Report;

    /// What its copies' destinations could not keep
    /// ([`Operation::omissions`]).
    fn omissions(&self) -> &Omissions;
}

impl<D: Dir> Run for Operation<D> {
    fn advance(&mut self, units: usize, buf: &mut Vec<u8>) -> Progress {
        Operation::advance(self, units, buf)
    }

    fn conflict(&self) -> Option<&Conflict> {
        Operation::conflict(self)
    }

    fn resolve(&mut self, decision: Decision) {
        Operation::resolve(self, decision);
    }

    fn report(&self) -> &Report {
        Operation::report(self)
    }

    fn omissions(&self) -> &Omissions {
        Operation::omissions(self)
    }
}

/// Whether a [`FileOp`] transfers or deletes, for the prompts and the
/// completion report.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum OpKind {
    /// Reproduce the source at the target, leaving the source in place.
    Copy,
    /// Put the source at the target: a rename on one volume, else a copy
    /// after which what was copied goes.
    Move,
    /// Remove the source and, for a directory, everything under it.
    Delete,
}

/// What one [`FileOp::advance`] call achieved.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OpProgress {
    /// The operation has ended; its summary says how.
    Done,
    /// A [`Conflict`] awaits a [`Decision`] through [`FileOp::resolve`].
    NeedsDecision,
    /// A step failed; the operation stopped with this error.
    Failed(OpError),
}

/// A started operation: the engine's own, run to its end or paused on an
/// overwrite question, and the buffer its bytes pass through.
pub struct FileOp {
    kind: OpKind,
    run: Box<dyn Run>,
    buf: Vec<u8>,
}

impl fmt::Debug for FileOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileOp")
            .field("kind", &self.kind)
            .field("conflict", &self.run.conflict())
            .field("report", self.run.report())
            .finish_non_exhaustive()
    }
}

impl FileOp {
    /// A copy of `src`, a path and the node it was listed as, to the planned
    /// `target`.
    ///
    /// # Errors
    ///
    /// The refusal opening the directory either path names.
    pub fn copy(fs: &mut dyn Fs, src: (&str, FileId), target: &str) -> Result<Self, OpError> {
        Self::started(OpKind::Copy, fs.transfer(Verb::Copy, src, target))
    }

    /// A move of `src` to the planned `target`: a rename where the volume
    /// allows it, a copy whose source then goes where it does not.
    ///
    /// # Errors
    ///
    /// As [`FileOp::copy`].
    pub fn move_to(fs: &mut dyn Fs, src: (&str, FileId), target: &str) -> Result<Self, OpError> {
        Self::started(OpKind::Move, fs.transfer(Verb::Move, src, target))
    }

    /// A delete of `path`, a path and the node it was listed as; a directory
    /// goes with everything under it, children before parents.
    ///
    /// # Errors
    ///
    /// The refusal opening the directory holding it.
    pub fn delete(fs: &mut dyn Fs, path: (&str, FileId)) -> Result<Self, OpError> {
        Self::started(OpKind::Delete, fs.remove(path))
    }

    fn started(kind: OpKind, run: Result<Box<dyn Run>, op::OpError>) -> Result<Self, OpError> {
        let run = run.map_err(OpError::Engine)?;
        Ok(Self {
            kind,
            run,
            buf: Vec::new(),
        })
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
        self.run.conflict()
    }

    /// Whether the user cancelled the operation's remaining steps; work
    /// already applied stayed applied.
    #[must_use]
    pub fn cancelled(&self) -> bool {
        self.run.report().cancelled
    }

    /// The entries a removal kept because they changed while it ran.
    #[must_use]
    pub fn kept(&self) -> &[String] {
        &self.run.report().kept
    }

    /// The extended attributes the copies so far could not carry, and why.
    #[must_use]
    pub fn omissions(&self) -> &Omissions {
        self.run.omissions()
    }

    /// Answer the paused question: overwrite removes the name asked about,
    /// skip leaves it and the source, cancel stops after giving the
    /// directories made their modes.
    pub fn resolve(&mut self, decision: Decision) {
        self.run.resolve(decision);
    }

    /// Run the operation until it ends, fails, or pauses on a question.
    pub fn advance(&mut self) -> OpProgress {
        loop {
            match self.run.advance(usize::MAX, &mut self.buf) {
                Progress::Yielded => {}
                Progress::Done => return OpProgress::Done,
                Progress::Paused => return OpProgress::NeedsDecision,
                Progress::Failed(error) => return OpProgress::Failed(OpError::Engine(error)),
            }
        }
    }

    /// The one-line completion report for the message line.
    #[must_use]
    pub fn summary(&self) -> String {
        let report = self.run.report();
        let verb = match self.kind {
            OpKind::Copy => "copied",
            OpKind::Move => "moved",
            OpKind::Delete => "deleted",
        };
        let noun = if report.done == 1 { "entry" } else { "entries" };
        let mut text = format!("{verb} {} {noun}", report.done);
        // Writing to a `String` cannot fail.
        if report.skipped > 0 {
            let _ = write!(text, ", {} skipped", report.skipped);
        }
        match report.kept.as_slice() {
            [] => {}
            [one] => {
                let _ = write!(text, ", {} kept: it changed meanwhile", quote_subject(one));
            }
            many => {
                let _ = write!(text, ", {} kept: they changed meanwhile", many.len());
            }
        }
        if report.cancelled {
            text.push_str(" (cancelled)");
        }
        text
    }
}
