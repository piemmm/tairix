//! The one resumable copy, move and removal over the tree seam, carried out a
//! bounded number of units at a time and pausing on a question about a taken
//! name, so a surface can slice it off its loop and ask the question there. A
//! unit is one entry acted on or one chunk of a file's bytes.
//!
//! Every entry is reached by name from a directory the operation holds, never
//! by a joined path, and changed through the node it was read from; the paths
//! an operation carries are spellings for its reports. A copy gives what it
//! makes the modes `cp` gives, a move keeps its source's mode and owner as
//! `mv` does, and a move across volumes removes, once its copy is whole and on
//! the medium, only what it copied while unchanged.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::{Errno, FileId, FileKind, FileStat, NodeExpect, RenameFlags, UnlinkFlags};
use tairix_path::join;
use tairix_util::fallible::{concatenated, grow_to, push};

use crate::attrs::{copy_attributes, AttrError, Attributes, Omissions};
use crate::data::{DataCopy, DataError, Sparse, Step};
use crate::mode::{carry_owner, file_mode, DirModes, Ownership, Settle, OWNER_RW, OWNER_RWX};
use crate::tree::{unlink_node, Cursor, Dir, Entry, Kind, Node, Reached, Roots};
use crate::within::{landing, lands_inside, Descent, Landing, Unresolved};
use crate::CHUNK_LEN;

/// How an operation names the replacement it builds beside a name it is to
/// replace: a fresh name in the same directory (`tairix_rt::staging_name`).
pub type Stager = fn(&str) -> Result<String, Errno>;

/// What a transfer does with its source.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Verb {
    /// Leave it where it is.
    Copy,
    /// Take it away: one rename on its own volume, else a copy after which
    /// what was copied goes.
    Move,
}

/// What a transfer does where the name a leaf takes is already taken.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Taken {
    /// Pause on a [`Conflict`]; a copy's directory meeting a directory merges
    /// into it, a move's replaces it only while it is empty.
    Ask,
    /// Fail and keep the name, whatever holds it: for a caller that chose
    /// free names.
    Refuse,
}

/// A name in a directory an operation holds, and the path its user knows
/// it by.
pub struct Named<D> {
    /// The directory holding the name.
    pub dir: D,
    /// The name.
    pub name: String,
    /// What the user calls it: a spelling for reports, never resolved.
    pub path: String,
    /// The node a listing showed under the name, or [`FileId::NONE`]: a
    /// source found holding another is refused as [`OpError::Changed`].
    pub seen: FileId,
}

impl<D: Dir> Named<D> {
    /// `path`'s last component, in the directory `roots` opens through the
    /// rest of it.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] for a path ending in no name, the open's
    /// refusal, or [`Errno::OutOfMemory`].
    pub fn parent<R: Roots<Dir = D>>(roots: &R, path: &str) -> Result<Self, Errno> {
        let (dir, name) = roots.parent(path)?;
        let path = concatenated(&[path]).ok_or(Errno::OutOfMemory)?;
        Ok(Self {
            dir,
            name,
            path,
            seen: FileId::NONE,
        })
    }
}

/// A leaf whose target name is taken, awaiting a [`Decision`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Conflict {
    /// The leaf being carried.
    pub source: String,
    /// The name it would take.
    pub target: String,
    /// What that name holds: a file or a link.
    pub occupant: Kind,
}

/// An answer to a [`Conflict`]; a cancel is taken at any time.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Decision {
    /// Remove what the name holds, only while it is what was asked about,
    /// and go on.
    Overwrite,
    /// Leave the name and the leaf as they are, and go on.
    Skip,
    /// Stop, the directories made given their modes first.
    Cancel,
}

/// Where [`Operation::advance`] left the operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Progress {
    /// It has ended, as its [`Report`] says.
    Done,
    /// Its units are spent and work remains.
    Yielded,
    /// A [`Conflict`] awaits [`Operation::resolve`].
    Paused,
    /// It stopped at this refusal, the directories made given their modes.
    Failed(OpError),
}

/// Why an operation stopped.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OpError {
    /// A directory would land on or inside itself.
    IntoItself,
    /// `path` holds a directory where a leaf goes, or a leaf where a
    /// directory goes.
    KindMismatch(String),
    /// `path` would not be looked at, opened, listed or read.
    Read(String, Errno),
    /// `path` would not be made, written, given its mode or owner, or put
    /// on the medium.
    Write(String, Errno),
    /// The move of `path` was refused.
    Rename(String, Errno),
    /// `path` would not be removed.
    Remove(String, Errno),
    /// `path` refused the attribute `key` for a reason that is no
    /// attribute's.
    Attribute {
        /// The node refusing it.
        path: String,
        /// The attribute.
        key: String,
        /// The refusal.
        errno: Errno,
    },
    /// `path` is no longer what it was looked at as, and was left alone.
    Changed(String),
}

impl OpError {
    /// The entry the refusal names, where it names one.
    #[must_use]
    pub fn path(&self) -> Option<&str> {
        let path = match self {
            Self::IntoItself => return None,
            Self::KindMismatch(path)
            | Self::Read(path, _)
            | Self::Write(path, _)
            | Self::Rename(path, _)
            | Self::Remove(path, _)
            | Self::Changed(path)
            | Self::Attribute { path, .. } => path,
        };
        (!path.is_empty()).then_some(path.as_str())
    }
}

/// What an operation has done.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Report {
    /// Entries put in place or removed: each file, link and directory made,
    /// the source renamed, each entry a removal took. A move's removal of what
    /// it copied counts nothing: its entries were counted as they were made.
    pub done: usize,
    /// Leaves left as they were on a [`Decision::Skip`].
    pub skipped: usize,
    /// Bytes of files accounted for: copied, or holes passed over.
    pub bytes: u64,
    /// Whether it was cancelled.
    pub cancelled: bool,
    /// What a removal kept: an entry put in its place since it was listed,
    /// or, after a move, one added or changed since it was copied.
    pub kept: Vec<String>,
}

/// A copy, move or removal under way.
pub struct Operation<D: Dir> {
    source: Named<D>,
    stage: Stage<D>,
    log: Log,
}

/// What an operation records as it goes, apart from where it stands.
#[derive(Default)]
struct Log {
    report: Report,
    omissions: Omissions,
    question: Option<Question>,
    cancelled: bool,
}

/// A question asked, the node it is about, and its answer once given.
struct Question {
    conflict: Conflict,
    occupant: FileId,
    answer: Option<Decision>,
}

enum Stage<D: Dir> {
    /// The source is looked at next.
    Look(Plan<D>),
    /// A move's rename onto its target is next.
    Rename(Box<Moving<D>>),
    Copy(Box<Copying<D>>),
    /// The operation's own removal, or a move's of what it copied.
    Remove(Box<Removal<D>>),
    /// What a copy made is settled before it ends: cancelled, or at the
    /// error.
    Winding(Box<Copying<D>>, Option<OpError>),
    Ended(Option<OpError>),
}

enum Plan<D> {
    Transfer {
        verb: Verb,
        taken: Taken,
        target: Named<D>,
        stage: Stager,
    },
    Remove,
}

struct Moving<D> {
    taken: Taken,
    target: Named<D>,
    /// The source as its look found it.
    found: FileStat,
    stage: Stager,
}

impl<D: Dir> Operation<D> {
    /// Copy or move `source` to `target`, a taken leaf name met as `taken`
    /// says, a replacement built under a name `stage` gives.
    #[must_use]
    pub fn transfer(
        verb: Verb,
        source: Named<D>,
        target: Named<D>,
        taken: Taken,
        stage: Stager,
    ) -> Self {
        Self::begun(
            source,
            Plan::Transfer {
                verb,
                taken,
                target,
                stage,
            },
        )
    }

    /// Remove `source`, and everything beneath a directory, its entries
    /// first; a link is removed as itself.
    #[must_use]
    pub fn remove(source: Named<D>) -> Self {
        Self::begun(source, Plan::Remove)
    }

    fn begun(source: Named<D>, plan: Plan<D>) -> Self {
        Self {
            source,
            stage: Stage::Look(plan),
            log: Log::default(),
        }
    }

    /// Carry out at most `units` units, a file's bytes passing through
    /// `buf`, which grows to at most [`CHUNK_LEN`] as a file needs it.
    pub fn advance(&mut self, units: usize, buf: &mut Vec<u8>) -> Progress {
        let mut left = units;
        loop {
            if let Stage::Ended(ending) = &self.stage {
                return ending.clone().map_or(Progress::Done, Progress::Failed);
            }
            if self.conflict().is_some() {
                return Progress::Paused;
            }
            let Some(rest) = left.checked_sub(1) else {
                return Progress::Yielded;
            };
            left = rest;
            let stage = core::mem::replace(&mut self.stage, Stage::Ended(None));
            self.stage = self.step(stage, buf);
        }
    }

    /// The question the operation waits on, if one is unanswered.
    #[must_use]
    pub fn conflict(&self) -> Option<&Conflict> {
        let question = self.log.question.as_ref()?;
        (question.answer.is_none() && !self.log.cancelled).then_some(&question.conflict)
    }

    /// Answer the question, or cancel at any time.
    pub fn resolve(&mut self, decision: Decision) {
        if decision == Decision::Cancel {
            self.log.cancelled = true;
            self.log.question = None;
        } else if let Some(question) = self.log.question.as_mut() {
            question.answer.get_or_insert(decision);
        }
    }

    /// What it has done so far.
    #[must_use]
    pub fn report(&self) -> &Report {
        &self.log.report
    }

    /// The extended attributes its copies' destinations could not keep.
    #[must_use]
    pub fn omissions(&self) -> &Omissions {
        &self.log.omissions
    }

    /// Whether it is a move across volumes removing what it copied: work its
    /// report does not count.
    #[must_use]
    pub fn removing(&self) -> bool {
        matches!(&self.stage, Stage::Remove(removal) if removal.moved)
    }

    fn step(&mut self, stage: Stage<D>, buf: &mut Vec<u8>) -> Stage<D> {
        let Self { source, log, .. } = self;
        match stage {
            Stage::Winding(copy, ending) => copy.wind(ending, source, log),
            Stage::Ended(ending) => Stage::Ended(ending),
            Stage::Copy(copy) if log.cancelled => Stage::Winding(copy, None),
            _ if log.cancelled => {
                log.report.cancelled = true;
                Stage::Ended(None)
            }
            Stage::Look(plan) => look(source, plan),
            Stage::Rename(moving) => rename(source, moving, log),
            Stage::Copy(mut copy) => match copy.unit(source, log, buf) {
                Ok(Flow::More) => Stage::Copy(copy),
                Ok(Flow::Whole) => (*copy).whole(),
                Err(error) => Stage::Winding(copy, Some(error)),
            },
            Stage::Remove(mut removal) => match removal.unit(source, log) {
                Ok(true) => Stage::Remove(removal),
                Ok(false) => Stage::Ended(None),
                Err(error) => Stage::Ended(Some(error)),
            },
        }
    }
}

fn look<D: Dir>(source: &Named<D>, plan: Plan<D>) -> Stage<D> {
    let found = match source
        .dir
        .open_node(&source.name)
        .and_then(|node| node.stat())
    {
        Ok(found) => found,
        Err(errno) => return Stage::Ended(Some(read(&source.path, errno))),
    };
    if replaced(source.seen, found.id) {
        return Stage::Ended(Some(OpError::Changed(source.path.clone())));
    }
    match plan {
        Plan::Remove => Stage::Remove(Box::new(Removal::asked(&found))),
        Plan::Transfer {
            verb: Verb::Move,
            taken,
            target,
            stage,
        } => Stage::Rename(Box::new(Moving {
            taken,
            target,
            found,
            stage,
        })),
        Plan::Transfer {
            verb: Verb::Copy,
            taken,
            target,
            stage,
        } => Stage::Copy(Box::new(Copying::new(
            (Verb::Copy, taken, stage),
            target,
            found,
        ))),
    }
}

/// Whether the node `found` is not the one listed as `listed`. A mount
/// point is listed with no identity of its own, so it is never taken for
/// another.
fn replaced(listed: FileId, found: FileId) -> bool {
    !listed.is_none() && found != listed
}

/// What one try of a move's rename came to.
enum Renamed {
    Done,
    /// The volumes differ: the source is copied across instead.
    Across,
    Asked,
    Withheld,
}

fn rename<D: Dir>(source: &Named<D>, moving: Box<Moving<D>>, log: &mut Log) -> Stage<D> {
    match try_rename(source, &moving, log) {
        Ok(Renamed::Done) => {
            log.report.done += 1;
            Stage::Ended(None)
        }
        Ok(Renamed::Across) => {
            let Moving {
                taken,
                target,
                found,
                stage,
            } = *moving;
            Stage::Copy(Box::new(Copying::new(
                (Verb::Move, taken, stage),
                target,
                found,
            )))
        }
        Ok(Renamed::Asked) => Stage::Rename(moving),
        Ok(Renamed::Withheld) => Stage::Ended(None),
        Err(error) => Stage::Ended(Some(error)),
    }
}

fn try_rename<D: Dir>(
    source: &Named<D>,
    moving: &Moving<D>,
    log: &mut Log,
) -> Result<Renamed, OpError> {
    let target = &moving.target;
    if let Some(question) = log.question.take() {
        match question.answer {
            Some(Decision::Skip) => {
                log.report.skipped += 1;
                return Ok(Renamed::Withheld);
            }
            Some(Decision::Overwrite) => {
                // One replacing rename: the name holds the occupant or the
                // source at every moment, whatever then fails.
                match target
                    .dir
                    .open_node(&target.name)
                    .and_then(|node| node.stat())
                {
                    Ok(now) if now.id == question.occupant => {
                        let renamed = source.dir.rename(
                            &source.name,
                            &target.dir,
                            &target.name,
                            RenameFlags::EXPECT_NODE,
                            Some(&NodeExpect::of_node(moving.found.id)),
                        );
                        return renamed_as(renamed, source, &target.dir);
                    }
                    Ok(_) => return taken_target(source, moving, log),
                    Err(Errno::NotFound) => {}
                    Err(errno) => return Err(read(&target.path, errno)),
                }
            }
            _ => {
                log.question = Some(question);
                return Ok(Renamed::Asked);
            }
        }
    }
    let expect = NodeExpect::of_node(moving.found.id);
    let renamed = source.dir.rename(
        &source.name,
        &target.dir,
        &target.name,
        RenameFlags::NO_REPLACE.union(RenameFlags::EXPECT_NODE),
        Some(&expect),
    );
    if renamed == Err(Errno::AlreadyExists) {
        return taken_target(source, moving, log);
    }
    renamed_as(renamed, source, &target.dir)
}

/// What a move's rename answering `renamed` means.
fn renamed_as<D: Dir>(
    renamed: Result<(), Errno>,
    source: &Named<D>,
    into: &D,
) -> Result<Renamed, OpError> {
    match renamed {
        Ok(()) => Ok(Renamed::Done),
        Err(Errno::CrossVolume) => Ok(Renamed::Across),
        Err(Errno::Stale) => Err(OpError::Changed(source.path.clone())),
        // A rename's only range refusal: a directory into its own subtree.
        Err(Errno::OutOfRange) if lands_inside(&source.dir, &source.name, into) => {
            Err(OpError::IntoItself)
        }
        Err(errno) => Err(OpError::Rename(source.path.clone(), errno)),
    }
}

/// A move's target is taken: a leaf over a leaf is asked about, a directory
/// over a directory left to the rename's own rule, which replaces only an
/// empty one.
fn taken_target<D: Dir>(
    source: &Named<D>,
    moving: &Moving<D>,
    log: &mut Log,
) -> Result<Renamed, OpError> {
    let target = &moving.target;
    if moving.taken == Taken::Refuse {
        return Err(OpError::Rename(source.path.clone(), Errno::AlreadyExists));
    }
    let occupant = match target
        .dir
        .open_node(&target.name)
        .and_then(|node| node.stat())
    {
        Ok(occupant) => occupant,
        Err(Errno::NotFound) => {
            return Err(OpError::Rename(source.path.clone(), Errno::AlreadyExists))
        }
        Err(errno) => return Err(read(&target.path, errno)),
    };
    let directory = moving.found.kind == FileKind::Directory;
    match (directory, occupant.kind == FileKind::Directory) {
        (false, false) => {
            log.question = Some(Question {
                conflict: Conflict {
                    source: source.path.clone(),
                    target: target.path.clone(),
                    occupant: Kind::of(occupant.kind),
                },
                occupant: occupant.id,
                answer: None,
            });
            Ok(Renamed::Asked)
        }
        (true, true) => {
            let expect = NodeExpect::of_node(moving.found.id);
            let renamed = source.dir.rename(
                &source.name,
                &target.dir,
                &target.name,
                RenameFlags::EXPECT_NODE,
                Some(&expect),
            );
            renamed_as(renamed, source, &target.dir)
        }
        _ => Err(OpError::KindMismatch(target.path.clone())),
    }
}

/// Whether a unit left more of a stage to do.
enum Flow {
    More,
    Whole,
}

/// What a move across volumes copied: an entry by its name, what it was as
/// it was copied — `None` for a leaf a skip withheld — and a directory's own
/// entries, sorted by name.
struct Copied {
    name: String,
    kind: Kind,
    expect: Option<NodeExpect>,
    entries: Vec<Copied>,
}

/// A copy, a transfer's own or a move's across volumes.
struct Copying<D: Dir> {
    verb: Verb,
    taken: Taken,
    stage: Stager,
    target: Named<D>,
    /// The source as its look found it.
    found: FileStat,
    /// The walk beneath a directory source; none for a leaf.
    cursor: Option<Cursor<D, D>>,
    /// The leaf in hand: the source itself, or the entry the walk is at.
    leaf: Option<Leaf<D::File>>,
    /// Each target directory the copy is inside, the source's own first.
    levels: Vec<Level<D>>,
    descent: Descent,
    begun: bool,
    /// What a move copied of its source, once the copy is whole.
    record: Option<Copied>,
}

/// A target directory the copy is inside.
struct Level<D> {
    dir: D,
    id: FileId,
    leave: Leave,
    volume: [u8; 16],
    /// What a move copied into it.
    copied: Vec<Copied>,
}

/// What a target directory is given once its entries are in.
enum Leave {
    /// Nothing: it was there before, and keeps its own mode.
    Merged,
    /// A copy's: its source's mode less the umask.
    Settle(Settle),
    /// A move's: the source directory's mode and owner, carried onto the
    /// directory as it was made.
    Carry {
        /// The source directory's.
        source: Ownership,
        /// The directory's as it was made.
        made: Ownership,
    },
}

struct Leaf<F> {
    from: Origin<F>,
    /// The file made and its copy so far, once made.
    made: Option<Filling<F>>,
    /// Where an overwrite builds it, to rename over what it replaces.
    placing: Option<Placing>,
}

/// A replacement built under a staging name, and the node it replaces.
struct Placing {
    staged: String,
    occupant: FileId,
}

enum Origin<F> {
    File {
        source: F,
        stat: FileStat,
    },
    /// A link's stored target, and its stat for a move, which carries its
    /// owner and removes it only while unchanged.
    Link {
        stored: String,
        stat: Option<FileStat>,
    },
}

impl<F> Origin<F> {
    const fn kind(&self) -> Kind {
        match self {
            Self::File { .. } => Kind::File,
            Self::Link { .. } => Kind::Link,
        }
    }
}

struct Filling<F> {
    file: F,
    copy: DataCopy,
}

/// Where the leaf in hand comes from and goes.
struct Spot<'s, D> {
    name: &'s str,
    path: &'s str,
    into: &'s D,
    as_name: &'s str,
    /// The source's spelling and its target's, between which every path
    /// beneath maps.
    roots: (&'s str, &'s str),
}

impl<D> Spot<'_, D> {
    fn target(&self) -> String {
        target_of(self.roots, self.path)
    }
}

/// What making a leaf's target came to.
enum Made {
    Filling,
    Link,
    Asked,
    Withheld,
}

/// Where a unit left the leaf in hand.
enum LeafEnd {
    More,
    /// It is done with, and this is what a move copied.
    Done(Option<Copied>),
}

impl<D: Dir> Copying<D> {
    fn new((verb, taken, stage): (Verb, Taken, Stager), target: Named<D>, found: FileStat) -> Self {
        Self {
            verb,
            taken,
            stage,
            target,
            found,
            cursor: None,
            leaf: None,
            levels: Vec::new(),
            descent: Descent::default(),
            begun: false,
            record: None,
        }
    }

    fn unit(
        &mut self,
        source: &Named<D>,
        log: &mut Log,
        buf: &mut Vec<u8>,
    ) -> Result<Flow, OpError> {
        if !self.begun {
            self.begun = true;
            return self.begin(source, log);
        }
        if self.leaf.is_some() {
            return self.leaf_unit(source, log, buf);
        }
        if self.cursor.is_some() {
            return self.walk_unit(source, log);
        }
        Ok(Flow::Whole)
    }

    fn begin(&mut self, source: &Named<D>, log: &mut Log) -> Result<Flow, OpError> {
        let kind = Kind::of(self.found.kind);
        if kind != Kind::Directory {
            self.leaf = Some(open_leaf(
                &source.dir,
                (source.name.as_str(), source.path.as_str()),
                (kind, self.found.id),
                self.verb,
            )?);
            return Ok(Flow::More);
        }
        let held = source
            .dir
            .open_dir(&source.name)
            .map_err(|errno| opened(errno, &source.path))?;
        if held.stat().map_err(|errno| read(&source.path, errno))?.id != self.found.id {
            return Err(OpError::Changed(source.path.clone()));
        }
        let cursor =
            Cursor::<D, D>::new(held, &source.path).map_err(|errno| read(&source.path, errno))?;
        let target = &self.target;
        let path = || target.path.clone();
        let mut existing = probe_dir(&target.dir, &target.name, self.taken, &path)?;
        match landing(cursor.root(), existing.as_ref().unwrap_or(&target.dir)) {
            Ok(Landing::Apart) => {}
            Ok(Landing::Same | Landing::Within) => return Err(OpError::IntoItself),
            Err(Unresolved::Source(errno)) => return Err(read(&source.path, errno)),
            Err(Unresolved::Target(errno)) => return Err(read(&target.path, errno)),
        }
        if self.verb == Verb::Move {
            if let Some(empty) = existing.take() {
                vacate(&target.dir, &target.name, &empty, &path)?;
            }
        }
        reserve(&mut self.levels)?;
        let (level, made) = match existing {
            Some(dir) => (merged(dir, &path)?, false),
            None => make_dir(
                &target.dir,
                &target.name,
                (self.verb, self.taken),
                &self.found,
                &path,
            )?,
        };
        let id = level.id;
        self.levels.push(level);
        if made {
            if let Some(level) = self.levels.last() {
                carry_attributes(cursor.root(), &level.dir, &source.path, &path, log)?;
            }
            log.report.done += 1;
        }
        self.descent
            .enter(self.found.id, id)
            .map_err(|_| out_of_memory())?;
        self.cursor = Some(cursor);
        Ok(Flow::More)
    }

    fn walk_unit(&mut self, source: &Named<D>, log: &mut Log) -> Result<Flow, OpError> {
        let roots = (source.path.as_str(), self.target.path.as_str());
        let Some(walk) = self.cursor.as_mut() else {
            return Ok(Flow::Whole);
        };
        let held = match walk.reach() {
            None => None,
            Some(Reached::Entry { dir, entry, path }) => {
                if entry.kind != Kind::Directory {
                    self.leaf = Some(open_leaf(
                        dir,
                        (entry.name.as_str(), path),
                        (entry.kind, entry.id),
                        self.verb,
                    )?);
                    return Ok(Flow::More);
                }
                Some(enter(
                    dir,
                    (entry, path),
                    (self.verb, self.taken),
                    roots,
                    (&mut self.levels, &mut self.descent),
                    log,
                )?)
            }
            Some(Reached::Left { entry, path, .. }) => {
                leave(
                    entry,
                    path,
                    self.verb,
                    roots,
                    (&mut self.levels, &mut self.descent),
                )?;
                return Ok(Flow::More);
            }
        };
        let Some(held) = held else {
            return self.finish(&source.name);
        };
        if let Err(errno) = walk.descend_into(held) {
            let path = walk.entry().map_or("", |(_, _, path)| path);
            return Err(read(path, errno));
        }
        Ok(Flow::More)
    }

    /// The walk has left the source's last entry: its own directory is given
    /// its mode, and a move puts the whole copy on the medium and records it
    /// under `name`.
    fn finish(&mut self, name: &str) -> Result<Flow, OpError> {
        let Some(root) = self.levels.pop() else {
            return Ok(Flow::Whole);
        };
        self.descent.leave();
        let write = |errno| OpError::Write(self.target.path.clone(), errno);
        conclude(&root).map_err(write)?;
        if self.verb == Verb::Move {
            root.dir.sync().map_err(write)?;
            self.record = Some(Copied {
                name: concatenated(&[name]).ok_or_else(out_of_memory)?,
                kind: Kind::Directory,
                expect: Some(NodeExpect::of_node(self.found.id)),
                entries: sorted(root.copied),
            });
        }
        Ok(Flow::Whole)
    }

    fn leaf_unit(
        &mut self,
        source: &Named<D>,
        log: &mut Log,
        buf: &mut Vec<u8>,
    ) -> Result<Flow, OpError> {
        let Self {
            verb,
            taken,
            stage,
            target,
            cursor,
            leaf,
            levels,
            record,
            ..
        } = self;
        let whole = cursor.is_none();
        let ended = {
            let Some(current) = leaf.as_mut() else {
                return Ok(Flow::More);
            };
            let spot = spot(source, target, cursor.as_ref(), levels)
                .ok_or_else(|| read(&source.path, Errno::OutOfRange))?;
            let done = if current.made.is_none() {
                match make_leaf(&spot, current, (*verb, *taken, *stage), log)? {
                    Made::Filling | Made::Asked => None,
                    Made::Link => {
                        Some((Kind::Link, finish_link(&spot, current, *verb, whole, log)?))
                    }
                    Made::Withheld => Some((current.from.kind(), None)),
                }
            } else if fill_leaf(&spot, current, log, buf)? {
                Some((Kind::File, finish_file(&spot, current, *verb, whole, log)?))
            } else {
                None
            };
            match done {
                None => LeafEnd::More,
                Some(_) if *verb == Verb::Copy => LeafEnd::Done(None),
                Some((kind, expect)) => LeafEnd::Done(Some(copied(spot.name, kind, expect)?)),
            }
        };
        let LeafEnd::Done(entry) = ended else {
            return Ok(Flow::More);
        };
        *leaf = None;
        if let Some(entry) = entry {
            match levels.last_mut() {
                Some(level) if !whole => {
                    if !push(&mut level.copied, entry) {
                        return Err(out_of_memory());
                    }
                }
                _ => *record = Some(entry),
            }
        }
        Ok(if whole { Flow::Whole } else { Flow::More })
    }

    /// The copy is whole: a move goes on to remove what it copied.
    fn whole(self) -> Stage<D> {
        let Self { verb, record, .. } = self;
        if verb == Verb::Copy {
            return Stage::Ended(None);
        }
        match record.and_then(Removal::moved) {
            Some(removal) => Stage::Remove(Box::new(removal)),
            None => Stage::Ended(None),
        }
    }

    /// Settle one more piece of what the copy made: the file it was writing
    /// removed first, then each directory, deepest first. Refusals here go
    /// unsaid: the ending is what is reported.
    fn wind(
        mut self: Box<Self>,
        ending: Option<OpError>,
        source: &Named<D>,
        log: &mut Log,
    ) -> Stage<D> {
        if let Some(leaf) = self.leaf.take() {
            if let Some(at) = spot(source, &self.target, self.cursor.as_ref(), &self.levels) {
                let name = leaf
                    .placing
                    .as_ref()
                    .map_or(at.as_name, |placing| placing.staged.as_str());
                if let Some(filling) = &leaf.made {
                    if let Ok(made) = filling.file.stat() {
                        let _ = unlink_node(at.into, name, made.id);
                    }
                } else if leaf.placing.is_some() {
                    // A staged link: only a link there is ever this one.
                    if let Ok(made) = at.into.open_node(name).and_then(|node| node.stat()) {
                        if made.kind == FileKind::Symlink {
                            let _ = unlink_node(at.into, name, made.id);
                        }
                    }
                }
            }
            return Stage::Winding(self, ending);
        }
        if let Some(level) = self.levels.pop() {
            self.descent.leave();
            let _ = conclude(&level);
            return Stage::Winding(self, ending);
        }
        if ending.is_none() {
            log.report.cancelled = true;
        }
        Stage::Ended(ending)
    }
}

/// Where the leaf in hand is: the source itself where no walk is under way,
/// else the entry the walk is at, going into the directory it is inside.
fn spot<'s, D: Dir>(
    source: &'s Named<D>,
    target: &'s Named<D>,
    cursor: Option<&'s Cursor<D, D>>,
    levels: &'s [Level<D>],
) -> Option<Spot<'s, D>> {
    let roots = (source.path.as_str(), target.path.as_str());
    let Some(cursor) = cursor else {
        return Some(Spot {
            name: &source.name,
            path: &source.path,
            into: &target.dir,
            as_name: &target.name,
            roots,
        });
    };
    let (_, entry, path) = cursor.entry()?;
    Some(Spot {
        name: &entry.name,
        path,
        into: &levels.last()?.dir,
        as_name: &entry.name,
        roots,
    })
}

/// Open the leaf `name` of `dir`, known as `path`, of the kind and identity
/// it was listed or looked at with.
fn open_leaf<D: Dir>(
    dir: &D,
    (name, path): (&str, &str),
    (kind, id): (Kind, FileId),
    verb: Verb,
) -> Result<Leaf<D::File>, OpError> {
    let from = if kind == Kind::File {
        let source = dir
            .open_file(name, false)
            .map_err(|errno| opened(errno, path))?;
        let stat = source.stat().map_err(|errno| read(path, errno))?;
        if replaced(id, stat.id) {
            return Err(OpError::Changed(String::from(path)));
        }
        Origin::File { source, stat }
    } else {
        let stat = match verb {
            Verb::Copy => None,
            Verb::Move => {
                let stat = dir
                    .open_node(name)
                    .and_then(|node| node.stat())
                    .map_err(|errno| opened(errno, path))?;
                if stat.kind != FileKind::Symlink || replaced(id, stat.id) {
                    return Err(OpError::Changed(String::from(path)));
                }
                Some(stat)
            }
        };
        let stored = dir.readlink(name).map_err(|errno| match errno {
            // The name holds no link now.
            Errno::OutOfRange => OpError::Changed(String::from(path)),
            errno => opened(errno, path),
        })?;
        Origin::Link { stored, stat }
    };
    Ok(Leaf {
        from,
        made: None,
        placing: None,
    })
}

/// Make the leaf's target, a question about a taken name answered first.
fn make_leaf<D: Dir>(
    spot: &Spot<'_, D>,
    leaf: &mut Leaf<D::File>,
    (verb, taken, stage): (Verb, Taken, Stager),
    log: &mut Log,
) -> Result<Made, OpError> {
    if let Some(question) = log.question.take() {
        match question.answer {
            Some(Decision::Skip) => {
                log.report.skipped += 1;
                return Ok(Made::Withheld);
            }
            // Built beside the occupant, which stays until its replacement
            // is whole and on the medium.
            Some(Decision::Overwrite) => match spot
                .into
                .open_node(spot.as_name)
                .and_then(|node| node.stat())
            {
                Ok(now) if now.id == question.occupant => {
                    let staged = stage(spot.as_name)
                        .map_err(|errno| OpError::Write(spot.target(), errno))?;
                    leaf.placing = Some(Placing {
                        staged,
                        occupant: question.occupant,
                    });
                }
                Ok(_) => return taken_leaf(spot, leaf, taken, log),
                Err(Errno::NotFound) => {}
                Err(errno) => return Err(OpError::Read(spot.target(), errno)),
            },
            _ => {
                log.question = Some(question);
                return Ok(Made::Asked);
            }
        }
    }
    let at = leaf
        .placing
        .as_ref()
        .map_or(spot.as_name, |placing| placing.staged.as_str());
    let made = match &leaf.from {
        Origin::File { stat, .. } => {
            let mode = match verb {
                Verb::Copy => file_mode(stat.mode),
                Verb::Move => OWNER_RW,
            };
            spot.into.create_file(at, mode).map(|file| {
                Some(Filling {
                    file,
                    copy: DataCopy::new(stat.size, Sparse::Auto),
                })
            })
        }
        Origin::Link { stored, .. } => spot.into.symlink(stored.as_bytes(), at).map(|()| None),
    };
    match made {
        Ok(Some(filling)) => {
            leaf.made = Some(filling);
            Ok(Made::Filling)
        }
        Ok(None) => Ok(Made::Link),
        Err(Errno::AlreadyExists) if leaf.placing.is_none() => taken_leaf(spot, leaf, taken, log),
        Err(errno) => Err(OpError::Write(spot.target(), errno)),
    }
}

/// Put the replacement `made`, built under `placing`'s staging name, over
/// the name it replaces in one rename, only while that name holds the node
/// asked about.
fn place<D: Dir>(spot: &Spot<'_, D>, placing: &Placing, made: FileId) -> Result<(), OpError> {
    match spot
        .into
        .open_node(spot.as_name)
        .and_then(|node| node.stat())
    {
        Ok(now) if now.id == placing.occupant => {}
        Err(Errno::NotFound) => {}
        Ok(_) => return Err(OpError::Changed(spot.target())),
        Err(errno) => return Err(OpError::Read(spot.target(), errno)),
    }
    spot.into
        .rename(
            &placing.staged,
            spot.into,
            spot.as_name,
            RenameFlags::EXPECT_NODE,
            Some(&NodeExpect::of_node(made)),
        )
        .map_err(|errno| OpError::Write(spot.target(), errno))
}

/// A leaf's target name is taken: asked about, or refused where a directory,
/// the source's only name, or a refusing caller holds it.
fn taken_leaf<D: Dir>(
    spot: &Spot<'_, D>,
    leaf: &Leaf<D::File>,
    taken: Taken,
    log: &mut Log,
) -> Result<Made, OpError> {
    let target = spot.target();
    let occupant = match spot
        .into
        .open_node(spot.as_name)
        .and_then(|node| node.stat())
    {
        Ok(occupant) => occupant,
        Err(Errno::NotFound) => return Err(OpError::Write(target, Errno::AlreadyExists)),
        Err(errno) => return Err(OpError::Read(target, errno)),
    };
    let kind = Kind::of(occupant.kind);
    if kind == Kind::Directory {
        return Err(OpError::KindMismatch(target));
    }
    // Removing a file's only name would leave its bytes held by the copy's
    // descriptor alone until the copy is whole.
    let only_name = matches!(&leaf.from, Origin::File { stat, .. }
        if !stat.id.is_none() && stat.id == occupant.id && occupant.nlink <= 1);
    if taken == Taken::Refuse || only_name {
        return Err(OpError::Write(target, Errno::AlreadyExists));
    }
    log.question = Some(Question {
        conflict: Conflict {
            source: String::from(spot.path),
            target,
            occupant: kind,
        },
        occupant: occupant.id,
        answer: None,
    });
    Ok(Made::Asked)
}

/// Copy the next chunk of the leaf's bytes, answering whether it is whole.
fn fill_leaf<D: Dir>(
    spot: &Spot<'_, D>,
    leaf: &mut Leaf<D::File>,
    log: &mut Log,
    buf: &mut Vec<u8>,
) -> Result<bool, OpError> {
    let (Origin::File { source, stat }, Some(filling)) = (&mut leaf.from, &mut leaf.made) else {
        return Ok(true);
    };
    let want = usize::try_from(stat.size).map_or(CHUNK_LEN, |len| len.clamp(1, CHUNK_LEN));
    if !grow_to(buf, want, 0) {
        return Err(out_of_memory());
    }
    match filling.copy.step(source, &mut filling.file, buf) {
        Ok(Step::Moved(count)) => {
            log.report.bytes = log.report.bytes.saturating_add(count);
            Ok(false)
        }
        Ok(Step::Done) => Ok(true),
        Err(DataError::Read(errno)) => Err(read(spot.path, errno)),
        Err(DataError::Write(errno)) => Err(OpError::Write(spot.target(), errno)),
    }
}

/// The leaf's bytes are in: its attributes follow, a move's mode and owner,
/// and, where it is the whole copy, its volume's commit. Answers what it
/// was as it was copied.
fn finish_file<D: Dir>(
    spot: &Spot<'_, D>,
    leaf: &Leaf<D::File>,
    verb: Verb,
    whole: bool,
    log: &mut Log,
) -> Result<Option<NodeExpect>, OpError> {
    let (Origin::File { source, stat }, Some(filling)) = (&leaf.from, &leaf.made) else {
        return Err(read(spot.path, Errno::OutOfRange));
    };
    carry_attributes(source, &filling.file, spot.path, &|| spot.target(), log)?;
    let write = |errno| OpError::Write(spot.target(), errno);
    let made = filling.file.stat().map_err(write)?;
    if verb == Verb::Move {
        carry_owner(Ownership::of(stat), Ownership::of(&made), &filling.file).map_err(write)?;
    }
    // On the medium before it replaces anything, and before a move removes
    // its source.
    if leaf.placing.is_some() || (whole && verb == Verb::Move) {
        filling.file.sync().map_err(write)?;
    }
    if let Some(placing) = &leaf.placing {
        place(spot, placing, made.id)?;
    }
    log.report.done += 1;
    Ok(Some(NodeExpect::of(stat)))
}

/// The leaf's link is made: a move carries the source link's owner onto it,
/// the very link made, and where it is the whole copy its volume commits.
fn finish_link<D: Dir>(
    spot: &Spot<'_, D>,
    leaf: &Leaf<D::File>,
    verb: Verb,
    whole: bool,
    log: &mut Log,
) -> Result<Option<NodeExpect>, OpError> {
    let write = |errno| OpError::Write(spot.target(), errno);
    let (stored, stat) = match &leaf.from {
        Origin::Link { stored, stat } => (stored, *stat),
        Origin::File { .. } => return Err(read(spot.path, Errno::OutOfRange)),
    };
    let at = leaf
        .placing
        .as_ref()
        .map_or(spot.as_name, |placing| placing.staged.as_str());
    // Reached by its name again: one swapped in a shared directory, holding
    // anything but the target just stored, is given nothing.
    let made = spot.into.open_node(at).map_err(write)?;
    let current = made.stat().map_err(write)?;
    if current.kind != FileKind::Symlink || current.size != stored.len() as u64 {
        return Err(OpError::Changed(spot.target()));
    }
    if let (Verb::Move, Some(stat)) = (verb, &stat) {
        carry_owner(Ownership::of(stat), Ownership::of(&current), &made).map_err(write)?;
    }
    if leaf.placing.is_some() || (whole && verb == Verb::Move) {
        spot.into.sync().map_err(write)?;
    }
    if let Some(placing) = &leaf.placing {
        place(spot, placing, current.id)?;
    }
    log.report.done += 1;
    Ok(stat.as_ref().map(NodeExpect::of))
}

fn copied(name: &str, kind: Kind, expect: Option<NodeExpect>) -> Result<Copied, OpError> {
    Ok(Copied {
        name: concatenated(&[name]).ok_or_else(out_of_memory)?,
        kind,
        expect,
        entries: Vec::new(),
    })
}

/// Go into the source directory `entry` of `dir`, at `path`, making or
/// merging into its target: answers the directory held, to descend.
fn enter<D: Dir>(
    dir: &D,
    (entry, path): (&Entry, &str),
    (verb, taken): (Verb, Taken),
    roots: (&str, &str),
    (levels, descent): (&mut Vec<Level<D>>, &mut Descent),
    log: &mut Log,
) -> Result<D, OpError> {
    let held = dir
        .open_dir(&entry.name)
        .map_err(|errno| opened(errno, path))?;
    let stat = held.stat().map_err(|errno| read(path, errno))?;
    if replaced(entry.id, stat.id) {
        return Err(OpError::Changed(String::from(path)));
    }
    if descent.refuses(stat.id) {
        return Err(OpError::IntoItself);
    }
    let target = || target_of(roots, path);
    reserve(levels)?;
    let (level, made) = {
        let into = &levels
            .last()
            .ok_or_else(|| read(path, Errno::OutOfRange))?
            .dir;
        // A move's own directories are fresh: only a copy merges.
        let existing = match verb {
            Verb::Copy => probe_dir(into, &entry.name, taken, &target)?,
            Verb::Move => None,
        };
        match existing {
            Some(existing) => (merged(existing, &target)?, false),
            None => make_dir(into, &entry.name, (verb, taken), &stat, &target)?,
        }
    };
    let id = level.id;
    levels.push(level);
    if made {
        if let Some(level) = levels.last() {
            carry_attributes(&held, &level.dir, path, &target, log)?;
        }
        log.report.done += 1;
    }
    descent.enter(stat.id, id).map_err(|_| out_of_memory())?;
    Ok(held)
}

/// The walk has left the directory `entry`, at `path`: its target is given
/// its mode, put on the medium where it lies on a volume of its own, and a
/// move records it.
fn leave<D: Dir>(
    entry: &Entry,
    path: &str,
    verb: Verb,
    roots: (&str, &str),
    (levels, descent): (&mut Vec<Level<D>>, &mut Descent),
) -> Result<(), OpError> {
    let Some(level) = levels.pop() else {
        return Ok(());
    };
    descent.leave();
    let write = |errno| OpError::Write(target_of(roots, path), errno);
    conclude(&level).map_err(write)?;
    // A move's copy must be on the medium before its source goes; a commit
    // reaches one volume, so each one met inside is put there too.
    if verb == Verb::Move
        && levels
            .last()
            .is_some_and(|parent| parent.volume != level.volume)
    {
        level.dir.sync().map_err(write)?;
    }
    if verb == Verb::Move {
        let directory = Copied {
            name: concatenated(&[&entry.name]).ok_or_else(out_of_memory)?,
            kind: Kind::Directory,
            expect: Some(NodeExpect::of_node(entry.id)),
            entries: sorted(level.copied),
        };
        let pushed = levels
            .last_mut()
            .is_some_and(|parent| push(&mut parent.copied, directory));
        if !pushed {
            return Err(out_of_memory());
        }
    }
    Ok(())
}

/// Remove the empty directory `existing`, named `name` in `into`, a move's
/// target, as a rename replaces one; one holding anything is refused.
fn vacate<D: Dir>(
    into: &D,
    name: &str,
    existing: &D,
    target: &dyn Fn() -> String,
) -> Result<(), OpError> {
    let entries = existing
        .entries()
        .map_err(|errno| OpError::Read(target(), errno))?;
    if !entries.is_empty() {
        return Err(OpError::Rename(target(), Errno::NotEmpty));
    }
    let id = existing
        .stat()
        .map_err(|errno| OpError::Read(target(), errno))?
        .id;
    into.unlink(
        name,
        UnlinkFlags::DIRECTORY.union(UnlinkFlags::EXPECT_NODE),
        Some(&NodeExpect::of_node(id)),
    )
    .map_err(|errno| match errno {
        Errno::NotEmpty | Errno::Stale => OpError::Changed(target()),
        errno => OpError::Remove(target(), errno),
    })
}

/// The directory already at the target name a directory takes, merged into
/// where `taken` asks; a leaf there is refused.
fn probe_dir<D: Dir>(
    into: &D,
    name: &str,
    taken: Taken,
    target: &dyn Fn() -> String,
) -> Result<Option<D>, OpError> {
    if taken == Taken::Refuse {
        return Ok(None);
    }
    match into.open_dir(name) {
        Ok(existing) => Ok(Some(existing)),
        Err(Errno::NotFound) => Ok(None),
        Err(Errno::NotADirectory) => Err(OpError::KindMismatch(target())),
        Err(errno) => Err(OpError::Read(target(), errno)),
    }
}

/// Make the target directory `name` in `into` for a source directory read
/// as `source`, holding its owner's read, write and search while it is
/// filled: answers its level, and whether it was made rather than found.
fn make_dir<D: Dir>(
    into: &D,
    name: &str,
    (verb, taken): (Verb, Taken),
    source: &FileStat,
    target: &dyn Fn() -> String,
) -> Result<(Level<D>, bool), OpError> {
    let modes = DirModes::new(match verb {
        Verb::Copy => source.mode,
        Verb::Move => OWNER_RWX,
    });
    let write = |errno| OpError::Write(target(), errno);
    let made = match into.make_dir(name, modes.asked()) {
        Ok(made) => made,
        Err(Errno::AlreadyExists) if taken == Taken::Ask && verb == Verb::Copy => {
            return match probe_dir(into, name, taken, target)? {
                Some(existing) => Ok((merged(existing, target)?, false)),
                None => Err(write(Errno::AlreadyExists)),
            };
        }
        Err(errno) => return Err(write(errno)),
    };
    let (born, settle) = modes.ready(&made).map_err(write)?;
    let leave = match verb {
        Verb::Copy => Leave::Settle(settle),
        Verb::Move => Leave::Carry {
            source: Ownership::of(source),
            made: Ownership::of(&born),
        },
    };
    Ok((
        Level {
            dir: made,
            id: born.id,
            leave,
            volume: born.id.volume,
            copied: Vec::new(),
        },
        true,
    ))
}

fn merged<D: Dir>(dir: D, target: &dyn Fn() -> String) -> Result<Level<D>, OpError> {
    let stat = dir.stat().map_err(|errno| OpError::Read(target(), errno))?;
    Ok(Level {
        dir,
        id: stat.id,
        leave: Leave::Merged,
        volume: stat.id.volume,
        copied: Vec::new(),
    })
}

/// Give the full target directory of `level` what it is owed.
fn conclude<D: Dir>(level: &Level<D>) -> Result<(), Errno> {
    match &level.leave {
        Leave::Merged => Ok(()),
        Leave::Settle(settle) => settle.finish(&level.dir),
        Leave::Carry { source, made } => carry_owner(*source, *made, &level.dir),
    }
}

/// Carry the extended attributes of `from`, known as `source`, to `to`,
/// noting what its volume could not keep.
fn carry_attributes<S: Node + ?Sized, T: Node + ?Sized>(
    from: &S,
    to: &T,
    source: &str,
    target: &dyn Fn() -> String,
    log: &mut Log,
) -> Result<(), OpError> {
    let left = copy_attributes(from, to, Attributes::Carry).map_err(|error| match error {
        AttrError::Read(errno) => read(source, errno),
        AttrError::Write { key, error } => OpError::Attribute {
            path: target(),
            key,
            errno: error,
        },
    })?;
    log.omissions.note(&left);
    Ok(())
}

/// A removal: the operation's own of its source and everything beneath, or a
/// move's of what it copied, each entry only while it is as it was read.
struct Removal<D: Dir> {
    /// What the source's own removal expects.
    expect: NodeExpect,
    directory: bool,
    /// A move's removal of what it copied: it counts nothing done, and a
    /// leaf goes only while unchanged.
    moved: bool,
    /// What a move copied into the source directory.
    record: Option<Vec<Copied>>,
    cursor: Option<Cursor<D, D>>,
    levels: Vec<Gone>,
    begun: bool,
}

/// A directory a removal is inside: whether everything in it went so far,
/// and what a move copied of it.
struct Gone {
    whole: bool,
    record: Option<Vec<Copied>>,
}

/// What becomes of an entry a removal reaches.
enum Fate {
    Unlink(UnlinkFlags, NodeExpect),
    Descend(FileId, Option<Vec<Copied>>),
    /// Added or changed since it was read: kept, and said.
    Keep,
    /// A skip's: kept unsaid.
    Withheld,
}

impl<D: Dir> Removal<D> {
    fn asked(found: &FileStat) -> Self {
        Self {
            expect: NodeExpect::of_node(found.id),
            directory: found.kind == FileKind::Directory,
            moved: false,
            record: None,
            cursor: None,
            levels: Vec::new(),
            begun: false,
        }
    }

    /// A move's removal of what `copied` records; none where a skip withheld
    /// the source itself.
    fn moved(copied: Copied) -> Option<Self> {
        Some(Self {
            expect: copied.expect?,
            directory: copied.kind == Kind::Directory,
            moved: true,
            record: Some(copied.entries),
            cursor: None,
            levels: Vec::new(),
            begun: false,
        })
    }

    /// One unit, answering whether more remains.
    fn unit(&mut self, source: &Named<D>, log: &mut Log) -> Result<bool, OpError> {
        if !self.begun {
            self.begun = true;
            return self.begin(source, log);
        }
        let expect = self.expect;
        let moved = self.moved;
        let Some(walk) = self.cursor.as_mut() else {
            return Ok(false);
        };
        let levels = &mut self.levels;
        let emptied = UnlinkFlags::DIRECTORY.union(UnlinkFlags::EXPECT_NODE);
        let (held, record) = match walk.reach() {
            None => {
                if levels.pop().is_some_and(|root| root.whole) {
                    let unlinked = source.dir.unlink(&source.name, emptied, Some(&expect));
                    gone(unlinked, &source.path, moved, levels, log)?;
                }
                return Ok(false);
            }
            Some(Reached::Left {
                dir, entry, path, ..
            }) => {
                if levels.pop().is_some_and(|level| level.whole) {
                    let expect = NodeExpect::of_node(entry.id);
                    let unlinked = dir.unlink(&entry.name, emptied, Some(&expect));
                    gone(unlinked, path, moved, levels, log)?;
                } else {
                    withhold(levels);
                }
                return Ok(true);
            }
            Some(Reached::Entry { dir, entry, path }) => match fate(levels.last_mut(), entry) {
                Fate::Unlink(flags, expect) => {
                    let unlinked = dir.unlink(&entry.name, flags, Some(&expect));
                    gone(unlinked, path, moved, levels, log)?;
                    return Ok(true);
                }
                Fate::Keep => {
                    keep(path, levels, log)?;
                    return Ok(true);
                }
                Fate::Withheld => {
                    withhold(levels);
                    return Ok(true);
                }
                Fate::Descend(id, record) => match dir.open_dir(&entry.name) {
                    Ok(held) => {
                        if held.stat().map_err(|errno| read(path, errno))?.id != id {
                            keep(path, levels, log)?;
                            return Ok(true);
                        }
                        (held, record)
                    }
                    Err(Errno::NotADirectory) => {
                        keep(path, levels, log)?;
                        return Ok(true);
                    }
                    Err(Errno::NotFound) => return Ok(true),
                    Err(errno) => return Err(read(path, errno)),
                },
            },
        };
        reserve(levels)?;
        if let Err(errno) = walk.descend_into(held) {
            let path = walk.entry().map_or("", |(_, _, path)| path);
            return Err(read(path, errno));
        }
        levels.push(Gone {
            whole: true,
            record,
        });
        Ok(true)
    }

    fn begin(&mut self, source: &Named<D>, log: &mut Log) -> Result<bool, OpError> {
        if !self.directory {
            let flags = if self.moved {
                UnlinkFlags::EXPECT_UNCHANGED
            } else {
                UnlinkFlags::EXPECT_NODE
            };
            let unlinked = source.dir.unlink(&source.name, flags, Some(&self.expect));
            if !self.moved && matches!(unlinked, Err(Errno::Stale | Errno::NotFound)) {
                return Err(OpError::Changed(source.path.clone()));
            }
            gone(unlinked, &source.path, self.moved, &mut self.levels, log)?;
            return Ok(false);
        }
        let held = match source.dir.open_dir(&source.name) {
            Ok(held) => held,
            Err(Errno::NotFound) if self.moved => return Ok(false),
            Err(Errno::NotADirectory) if self.moved => {
                return keep(&source.path, &mut self.levels, log).map(|()| false)
            }
            Err(errno) => return Err(opened(errno, &source.path)),
        };
        if held.stat().map_err(|errno| read(&source.path, errno))?.id != self.expect.id {
            return if self.moved {
                keep(&source.path, &mut self.levels, log).map(|()| false)
            } else {
                Err(OpError::Changed(source.path.clone()))
            };
        }
        let cursor =
            Cursor::<D, D>::new(held, &source.path).map_err(|errno| read(&source.path, errno))?;
        reserve(&mut self.levels)?;
        self.levels.push(Gone {
            whole: true,
            record: self.record.take(),
        });
        self.cursor = Some(cursor);
        Ok(true)
    }
}

/// What becomes of `entry`, inside the directory `level` stands for: what
/// its listing read, for the operation's own removal; what a move copied of
/// it, for a move's.
fn fate(level: Option<&mut Gone>, entry: &Entry) -> Fate {
    let Some(level) = level else {
        return Fate::Withheld;
    };
    let Some(record) = level.record.as_mut() else {
        return if entry.kind == Kind::Directory {
            Fate::Descend(entry.id, None)
        } else {
            Fate::Unlink(UnlinkFlags::EXPECT_NODE, NodeExpect::of_node(entry.id))
        };
    };
    let Ok(at) = record.binary_search_by(|copied| copied.name.as_str().cmp(entry.name.as_str()))
    else {
        return Fate::Keep;
    };
    let copied = &mut record[at];
    match copied.expect {
        None => Fate::Withheld,
        Some(_) if copied.kind != entry.kind => Fate::Keep,
        Some(expect) if copied.kind == Kind::Directory => {
            Fate::Descend(expect.id, Some(core::mem::take(&mut copied.entries)))
        }
        Some(expect) => Fate::Unlink(UnlinkFlags::EXPECT_UNCHANGED, expect),
    }
}

/// What a removal's unlink of `path` came to: gone, kept as changed
/// meanwhile, or a refusal.
fn gone(
    unlinked: Result<(), Errno>,
    path: &str,
    moved: bool,
    levels: &mut [Gone],
    log: &mut Log,
) -> Result<(), OpError> {
    match unlinked {
        Ok(()) => {
            if !moved {
                log.report.done += 1;
            }
            Ok(())
        }
        Err(Errno::NotFound) => Ok(()),
        Err(Errno::Stale | Errno::NotEmpty) => keep(path, levels, log),
        Err(errno) => Err(OpError::Remove(String::from(path), errno)),
    }
}

/// Keep `path` and say so; the directory holding it stays too.
fn keep(path: &str, levels: &mut [Gone], log: &mut Log) -> Result<(), OpError> {
    withhold(levels);
    if concatenated(&[path]).is_some_and(|kept| push(&mut log.report.kept, kept)) {
        Ok(())
    } else {
        Err(out_of_memory())
    }
}

/// The directory a removal is in keeps something.
fn withhold(levels: &mut [Gone]) {
    if let Some(level) = levels.last_mut() {
        level.whole = false;
    }
}

/// Where the source's `path`, at or beneath its own spelling, lands beneath
/// the target's: `(source, target)` are the two.
fn target_of((source, target): (&str, &str), path: &str) -> String {
    match path
        .get(source.len()..)
        .map(|rest| rest.trim_start_matches('/'))
    {
        Some(rest) if !rest.is_empty() => join(target, rest),
        _ => String::from(target),
    }
}

fn sorted(mut entries: Vec<Copied>) -> Vec<Copied> {
    entries.sort_unstable_by(|a, b| a.name.cmp(&b.name));
    entries
}

fn reserve<T>(levels: &mut Vec<T>) -> Result<(), OpError> {
    levels.try_reserve(1).map_err(|_| out_of_memory())
}

/// A refusal opening `path`: one saying it is no longer what its listing or
/// look said is a change.
fn opened(errno: Errno, path: &str) -> OpError {
    match errno {
        Errno::NotFound | Errno::NotADirectory | Errno::IsADirectory | Errno::LinkLoop => {
            OpError::Changed(String::from(path))
        }
        errno => read(path, errno),
    }
}

fn read(path: &str, errno: Errno) -> OpError {
    OpError::Read(String::from(path), errno)
}

/// The heap refused the operation's own record; spelled with no path, which
/// would need the heap too.
fn out_of_memory() -> OpError {
    OpError::Read(String::new(), Errno::OutOfMemory)
}

#[cfg(all(test, feature = "test-support"))]
#[path = "op_tests.rs"]
mod tests;
