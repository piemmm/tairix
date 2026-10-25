//! The move engine: resolve each source's destination and rename it there.
//! A rename that would cross a filesystem boundary instead builds the
//! source's copy under a staging name beside the destination, puts it on the
//! medium, renames it into place, and only then removes what it copied — so
//! the destination always names either what it held or the whole source, and
//! no failure or crash loses either.
//!
//! Every step goes through nodes the move holds open: the source is opened
//! without following a link, the copy is made owner-only and filled through
//! its own descriptor, it is put in place only while its staging name still
//! holds it, and only the entries it copied are removed, each only while it is
//! unchanged — so a name swapped for a link, or a file added or appended
//! meanwhile, is never written through, emptied or lost.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::fs::FileKind;
use tairix_abi::{Errno, FileId, FileStat, NodeExpect, RenameFlags, UnlinkFlags};
use tairix_fscopy::tree::{walk, Entry, Step, Visitor};
use tairix_fscopy::{
    copy_attributes, landing, lands_inside, AttrError, Attributes, Created, DataCopy, DataError,
    Descent, DirModes, Landing, Omissions, Ownership, Sparse, CHUNK_LEN, OWNER_RW, OWNER_RWX,
};
use tairix_hash::BuildSipHash13;
use tairix_help::{own_short_help, HelpSource};
use tairix_path::{join, leaf_name};
use tairix_util::fallible::concatenated;
use tairix_util::output::Carried;
use tairix_util::quote::quote;

use crate::command::{Clobber, Command, Options, TargetMode};
use crate::error::MvError;
use crate::io::{Dir, Kind, Node, Output, Prompt, Roots, Staging};

/// The usage banner the short-help switches print when `mv`'s own Help tree
/// is unavailable.
pub const USAGE: &str = "\
usage: mv [-finvT] [-t dir] [--] source... dest

  -f, --force                never prompt before overwriting
  -i, --interactive          ask before overwriting an existing destination
  -n, --no-clobber           never overwrite an existing destination
  -v, --verbose              report each move
  -t dir, --target-directory=dir
                             move every source into dir
  -T, --no-target-directory  treat dest as a normal file (one source)
  -h, -?, --help             show this message

With one source and a non-directory dest, the source is moved to dest. When
dest is an existing directory (always, with more than one source) each source
is moved into it under its base name. `--` ends option parsing: every later
argument is a path.

A move to another volume builds the copy beside the destination — each file
with its holes, extended attributes, mode and owner, each link as a link —
puts it on the disk, renames it into place, and only then removes what it
copied, so the destination holds what it held until the move is whole. An
entry that changed meanwhile is kept and reported. Attributes the destination
cannot keep are reported in one warning at the end.
";

/// `mv`'s own command word: the short-help switches render its own Help
/// document through the same engine as any other command's.
const OWN_WORD: &str = "mv";

/// What a run reaches outside itself through.
pub struct Seams<'a, R: Roots> {
    /// Where the operands are opened from.
    pub fs: &'a R,
    /// The names copies across volumes are built under.
    pub staging: &'a dyn Staging,
    /// The `-i` question.
    pub prompt: &'a dyn Prompt,
    /// The tool's own `Help/` tree, read by the short-help switches.
    pub help: &'a dyn HelpSource,
    /// The `-v` reports, the usage banner and the information stream.
    pub out: &'a dyn Output,
    /// The diagnostics: each failure carried past, and the end-of-run warning
    /// about attributes left behind.
    pub diagnostics: &'a dyn Output,
    /// The process's hash key, which the record of the destinations a run
    /// has put in place is kept under, since their names may be a hostile
    /// volume's to choose.
    pub keys: BuildSipHash13,
}

/// Run one [`Command`] through `seams`. `locale` is the user's `LANG`
/// preference, if set.
///
/// Each source is renamed onto its destination. When the destination is an
/// existing directory — and always with more than one source or `-t` — each
/// source is moved into it under its base name; `-T` uses the destination
/// exactly as given. An existing destination is replaced by default (and
/// under `-f`), kept under `-n`, and asked about under `-i` (a declined
/// question skips that move without error) — save one an earlier operand put
/// there, which a non-directory never replaces. A directory replaces only an
/// empty directory, nothing else replaces a directory, and a directory never
/// moves into its own subtree. `-v` reports each move.
///
/// A rename that would cross a filesystem boundary moves the source across
/// as the module describes, so the destination is replaced in one step.
///
/// A failed operand is reported on the diagnostics stream in GNU's words and
/// the next is still moved, as GNU's are, answering `Ok(false)` at the end;
/// every operand moved answers `Ok(true)`.
///
/// # Errors
///
/// * [`MvError::Usage`] — more than one source under `-T`.
/// * [`MvError::Target`] — the `-t` operand, or the destination of several
///   sources, is missing or not a directory.
/// * [`MvError::MemoryExhausted`] — the record of what the run put in place,
///   or of what a move across volumes copied, could not grow.
/// * [`MvError::Prompt`] — a confirmation could not be read (never treated
///   as consent).
/// * [`MvError::Output`] — writing the usage banner, a `-v` report or a
///   diagnostic failed.
///
/// The rest are reported per operand: [`MvError::Stat`] — an operand could
/// not be inspected; [`MvError::Rename`] — the move was refused;
/// [`MvError::IntoItself`], [`MvError::OverwriteDirectory`],
/// [`MvError::OverwriteNonDirectory`] — the move breaks the rule a rename
/// keeps; [`MvError::JustCreated`] — an earlier operand put the destination
/// there; [`MvError::Read`], [`MvError::Create`], [`MvError::Write`],
/// [`MvError::Attribute`] — a move across volumes failed before the
/// destination was replaced, the source kept; [`MvError::Remove`] — a copied
/// entry could not be removed after its move; [`MvError::Changed`] — a
/// source entry changed during its move and was kept.
///
/// Attributes a destination could not keep are reported once, after the
/// last move and before any failure that ended the run: a warning on the
/// diagnostics stream and an advisory record on the information stream.
pub fn run<R: Roots>(
    command: Command,
    locale: Option<&str>,
    seams: &Seams<'_, R>,
) -> Result<bool, MvError> {
    match command {
        Command::Help => short_help(locale, seams.help, seams.out).map(|()| true),
        Command::Move {
            options,
            sources,
            dest,
        } => {
            let mut mover = Mover {
                options,
                fs: seams.fs,
                staging: seams.staging,
                prompt: seams.prompt,
                out: seams.out,
                carried: Carried::new(seams.diagnostics, OWN_WORD),
                omissions: Omissions::default(),
                descent: Descent::default(),
                created: Created::new(seams.keys),
                buf: Vec::new(),
            };
            let outcome = mover.move_all(&sources, &dest);
            if let Some(report) = mover.omissions.report(OWN_WORD) {
                seams.out.info(&report.record);
                // The warning is advisory: a stream refusing it changes nothing.
                let _ = seams
                    .diagnostics
                    .write_all(format!("{OWN_WORD}: warning: {}\n", report.summary).as_bytes());
            }
            outcome
        }
    }
}

/// Render `mv`'s own short help (`NAME` + `SYNOPSIS` + compact `OPTIONS`)
/// from its own Help tree through the one shared engine; when no document
/// can be served (a build without the bundle's documents) the usage banner
/// stands in — the tool's own text, not fabricated help content — so `-h`
/// never fails.
fn short_help(
    locale: Option<&str>,
    help: &dyn HelpSource,
    out: &dyn Output,
) -> Result<(), MvError> {
    let bytes =
        own_short_help(help, locale, OWN_WORD).unwrap_or_else(|| String::from(USAGE).into_bytes());
    out.write_all(&bytes).map_err(MvError::Output)
}

/// What one rename onto a destination came to.
enum Placement {
    /// It was carried out.
    Renamed,
    /// The two names are on different volumes.
    CrossVolume,
    /// The destination that appeared since the look was kept, as a declined
    /// `-i` question says.
    Kept,
    /// The kernel refused it.
    Refused(Errno),
}

/// One operand's move: its source and destination, each held as the
/// directory naming it and that name.
struct Move<'m, D> {
    source: &'m str,
    source_dir: &'m D,
    source_name: &'m str,
    /// The source as it was looked at, never through a link.
    found: &'m FileStat,
    kind: Kind,
    target: &'m str,
    target_dir: &'m D,
    target_name: &'m str,
}

/// The node a copy across volumes is built in, held from its making to its
/// placing, and what it was made as.
struct Stage<D: Dir> {
    held: Staged<D>,
    made: FileStat,
}

enum Staged<D: Dir> {
    Directory(D),
    File(D::File),
    Link(D::File),
}

impl<D: Dir> Stage<D> {
    fn node(&self) -> &dyn Node {
        match &self.held {
            Staged::Directory(dir) => dir,
            Staged::File(file) | Staged::Link(file) => file,
        }
    }
}

/// What a move across volumes copied: an entry by the name it had, what it
/// was when it was copied, and a directory's own entries, sorted by name.
struct Copied {
    name: String,
    kind: Kind,
    expect: NodeExpect,
    entries: Vec<Copied>,
}

impl Copied {
    fn entry(&self, name: &str) -> Option<&Self> {
        self.entries
            .binary_search_by(|copied| copied.name.as_str().cmp(name))
            .ok()
            .map(|at| &self.entries[at])
    }
}

/// One `mv` run: its options, its seams, the walk of a tree moving across,
/// and what those moves' destinations could not keep.
struct Mover<'a, R: Roots> {
    options: Options,
    fs: &'a R,
    staging: &'a dyn Staging,
    prompt: &'a dyn Prompt,
    out: &'a dyn Output,
    /// The operands' failures, reported and carried past.
    carried: Carried<'a>,
    /// What the destinations across volumes could not keep, reported once.
    omissions: Omissions,
    /// The directories the walk is inside, so it never enters one again.
    descent: Descent,
    /// The destinations earlier operands put in place.
    created: Created,
    /// The one buffer every relocated file's bytes pass through.
    buf: Vec<u8>,
}

impl<R: Roots> Mover<'_, R> {
    /// Move every source to `dest`, deciding per source whether the
    /// destination is `dest` itself or a child of `dest` named after the
    /// source. A destination is probed through a final link: a directory
    /// reached by one is still the directory the moves land in.
    fn move_all(&mut self, sources: &[String], dest: &str) -> Result<bool, MvError> {
        let dest_kind = self.look(dest, true)?.map(|stat| Kind::of(stat.kind));
        let dest_is_dir = match self.options.target_mode {
            // `-t`: the destination must be an existing directory.
            TargetMode::Directory => match dest_kind {
                Some(Kind::Directory) => true,
                Some(_) => return Err(target_error(dest, true, Errno::NotADirectory)),
                None => return Err(target_error(dest, true, Errno::NotFound)),
            },
            // `-T`: the destination is a normal file for exactly one source.
            TargetMode::NoDirectory => {
                if sources.len() > 1 {
                    return Err(MvError::Usage);
                }
                false
            }
            TargetMode::Inferred => {
                // More than one source can only land inside a directory.
                if sources.len() > 1 && dest_kind != Some(Kind::Directory) {
                    let errno = dest_kind.map_or(Errno::NotFound, |_| Errno::NotADirectory);
                    return Err(target_error(dest, false, errno));
                }
                dest_kind == Some(Kind::Directory)
            }
        };
        for source in sources {
            let target = if dest_is_dir {
                join(dest, leaf_name(source))
            } else {
                String::from(dest)
            };
            let moved = self.move_operand(source, &target);
            self.carried.absorb(moved)?;
        }
        Ok(self.carried.clean())
    }

    /// What `path` names, a final link followed only when `follow`; a missing
    /// path [`None`].
    fn look(&self, path: &str, follow: bool) -> Result<Option<FileStat>, MvError> {
        absent_as_none(
            path,
            self.fs.open_node(path, follow).and_then(|node| node.stat()),
        )
    }

    /// Move one named source operand, a link as itself, to `target`.
    fn move_operand(&mut self, source: &str, target: &str) -> Result<(), MvError> {
        let (source_dir, source_name) = self
            .fs
            .parent(source)
            .map_err(|errno| stat_error(source, errno))?;
        let found = source_dir
            .open_node(&source_name)
            .and_then(|node| node.stat())
            .map_err(|errno| stat_error(source, errno))?;
        let (target_dir, target_name) = self
            .fs
            .parent(target)
            .map_err(|errno| stat_error(target, errno))?;
        let moving = Move {
            source,
            source_dir: &source_dir,
            source_name: &source_name,
            found: &found,
            kind: Kind::of(found.kind),
            target,
            target_dir: &target_dir,
            target_name: &target_name,
        };
        let occupant = absent_as_none(
            target,
            target_dir
                .open_node(&target_name)
                .and_then(|node| node.stat()),
        )?;
        let guarded = moving.kind != Kind::Directory;
        if guarded && occupant.is_some_and(|there| self.created.holds(there.id, target)) {
            return Err(MvError::JustCreated {
                source: String::from(source),
                target: String::from(target),
            });
        }
        if occupant.is_some() && !self.may_replace(target)? {
            self.carried.fail_quietly();
            return Ok(());
        }
        // Under `-n` or `-i` nothing the look did not decide about may be
        // replaced: a destination created since is decided as if it had been
        // there, with the kernel refusing the move rather than destroying it.
        let flags = if occupant.is_some() || self.options.clobber == Clobber::Overwrite {
            RenameFlags::empty()
        } else {
            RenameFlags::NO_REPLACE
        };
        let expect = NodeExpect::of_node(found.id);
        let placed = match self.rename_agreed(
            &source_dir,
            &source_name,
            &expect,
            (&target_dir, target_name.as_str(), target),
            flags,
        )? {
            Placement::Kept => None,
            Placement::Renamed => {
                self.report(format_args!(
                    "renamed {} -> {}",
                    quote(source),
                    quote(target)
                ))?;
                Some(found.id)
            }
            Placement::CrossVolume => self.relocate(&moving, flags)?,
            Placement::Refused(errno) => return Err(refusal(&moving, errno)),
        };
        match placed {
            None => self.carried.fail_quietly(),
            Some(id) if guarded => self
                .created
                .note(id, target)
                .map_err(|_| MvError::MemoryExhausted)?,
            Some(_) => {}
        }
        Ok(())
    }

    /// Rename `from`, while it names the node `expect` names, onto `name` in
    /// `dir` under `flags`, asking again about a destination that appeared
    /// since the look.
    fn rename_agreed(
        &self,
        from_dir: &R::Dir,
        from: &str,
        expect: &NodeExpect,
        (dir, name, target): (&R::Dir, &str, &str),
        flags: RenameFlags,
    ) -> Result<Placement, MvError> {
        let expecting = flags.union(RenameFlags::EXPECT_NODE);
        let renamed = match from_dir.rename(from, dir, name, expecting, Some(expect)) {
            Err(Errno::AlreadyExists) if flags.refuses_replace() => {
                if !self.may_replace(target)? {
                    return Ok(Placement::Kept);
                }
                from_dir.rename(from, dir, name, RenameFlags::EXPECT_NODE, Some(expect))
            }
            other => other,
        };
        Ok(match renamed {
            Ok(()) => Placement::Renamed,
            Err(Errno::CrossVolume) => Placement::CrossVolume,
            Err(errno) => Placement::Refused(errno),
        })
    }

    /// Whether the existing `target` may be replaced: by default and under
    /// `-f` always, and on the user's answer under `-i`; `-n` keeps it and
    /// says so, as GNU's does.
    ///
    /// # Errors
    ///
    /// [`MvError::NotReplaced`] under `-n`, carried past as a failure.
    fn may_replace(&self, target: &str) -> Result<bool, MvError> {
        match self.options.clobber {
            Clobber::Overwrite => Ok(true),
            Clobber::Skip => Err(MvError::NotReplaced(String::from(target))),
            Clobber::Prompt => self
                .prompt
                .confirm(&format!("overwrite {}?", quote(target)))
                .map_err(MvError::Prompt),
        }
    }

    /// Move the source across a filesystem boundary onto the target,
    /// replacing it in one rename once the copy is whole and on the medium;
    /// `flags` is the replacement the user agreed to. The node put in place,
    /// or `None` when a destination that appeared meanwhile kept it out.
    fn relocate(
        &mut self,
        m: &Move<'_, R::Dir>,
        flags: RenameFlags,
    ) -> Result<Option<FileId>, MvError> {
        let held = match m.kind {
            Kind::Directory => Some(held_source(m)?),
            Kind::File | Kind::Link => None,
        };
        admit(m, held.as_ref())?;
        let staged = self
            .staging
            .staging_name(m.target_name)
            .map_err(|errno| create_error(m.target, errno))?;
        // A staging name another holds is refused here, before there is
        // anything of the mover's to discard.
        let mut stage = self.make(m, &staged)?;
        let built = self.fill(m, held.as_ref(), &mut stage).and_then(|copied| {
            self.place(m, &staged, &stage, flags)
                .map(|placed| (copied, placed))
        });
        let copied = match built {
            Ok((copied, true)) => copied,
            Ok((_, false)) => {
                discard(m.target_dir, &staged, &stage);
                return Ok(None);
            }
            Err(err) => {
                discard(m.target_dir, &staged, &stage);
                return Err(err);
            }
        };
        // What it copied goes only once its new name is on the medium too.
        stage
            .node()
            .sync()
            .map_err(|errno| write_error(m.target, errno))?;
        self.copied(m.source, m.kind, m.target)?;
        self.remove_copied(m, held.as_ref(), &copied)?;
        Ok(Some(stage.made.id))
    }

    /// Make the node `staged` beside the target that the source's copy is
    /// built in, its owner's alone until it is filled, refusing a name that is
    /// already taken, and hold it.
    fn make(&mut self, m: &Move<'_, R::Dir>, staged: &str) -> Result<Stage<R::Dir>, MvError> {
        let held = match m.kind {
            Kind::File => m
                .target_dir
                .create_file(staged, OWNER_RW)
                .map(Staged::<R::Dir>::File),
            Kind::Directory => m
                .target_dir
                .make_dir(staged, OWNER_RWX)
                .map(Staged::<R::Dir>::Directory),
            Kind::Link => {
                let stored = m
                    .source_dir
                    .readlink(m.source_name)
                    .map_err(|errno| read_error(m.source, errno))?;
                m.target_dir
                    .symlink(stored.as_bytes(), staged)
                    .and_then(|()| {
                        m.target_dir
                            .open_node(staged)
                            .inspect_err(|_| forsake(m.target_dir, staged))
                    })
                    .map(Staged::<R::Dir>::Link)
            }
        }
        .map_err(|errno| create_error(m.target, errno))?;
        let made = match &held {
            Staged::Directory(dir) => DirModes::new(OWNER_RWX).ready(dir).map(|(born, _)| born),
            Staged::File(file) | Staged::Link(file) => file.stat(),
        };
        let made = made.map_err(|errno| {
            forsake(m.target_dir, staged);
            create_error(m.target, errno)
        })?;
        let stage = Stage { held, made };
        if m.kind == Kind::Directory {
            let reported = self.report(format_args!("created directory {}", quote(m.target)));
            if let Err(failure) = reported {
                discard(m.target_dir, staged, &stage);
                return Err(failure);
            }
        }
        Ok(stage)
    }

    /// Fill the staged node with what the source holds, through its
    /// descriptor, answering the record of what was copied; a link holds
    /// nothing more.
    fn fill(
        &mut self,
        m: &Move<'_, R::Dir>,
        held: Option<&R::Dir>,
        stage: &mut Stage<R::Dir>,
    ) -> Result<Copied, MvError> {
        let made = stage.made;
        match (&mut stage.held, held) {
            (Staged::File(file), _) => {
                let expect = self.fill_file(
                    (m.source_dir, m.source_name, m.source),
                    Some(m.found),
                    file,
                    m.target,
                )?;
                leaf(m.source_name, Kind::File, expect)
            }
            (Staged::Link(link), _) => {
                // A link made since, at the staged name, is not the copy.
                let stat = link.stat().map_err(|errno| create_error(m.target, errno))?;
                if stat.kind != FileKind::Symlink || stat.size != m.found.size {
                    return Err(create_error(m.target, Errno::Stale));
                }
                carry_owner(m.found, &stat, link, m.target)?;
                leaf(m.source_name, Kind::Link, NodeExpect::of(m.found))
            }
            (Staged::Directory(dir), Some(source)) => self.fill_directory(m, source, dir, &made),
            (Staged::Directory(_), None) => Err(read_error(m.source, Errno::NotADirectory)),
        }
    }

    /// Fill the staged directory `made` with the source directory `source`'s
    /// entries through a walk of the held source, then its attributes, owner
    /// and permission bits; the bits last, so a directory its owner may not
    /// write still takes its entries.
    fn fill_directory(
        &mut self,
        m: &Move<'_, R::Dir>,
        source: &R::Dir,
        made: &R::Dir,
        made_stat: &FileStat,
    ) -> Result<Copied, MvError> {
        self.descent
            .enter(m.found.id, made_stat.id)
            .map_err(|_| MvError::MemoryExhausted)?;
        let mut fill = Fill {
            mover: &mut *self,
            root: made,
            base: FillLevel {
                made: None,
                copied: Vec::new(),
            },
            spelled: (m.source.len(), m.target),
            levels: Vec::new(),
        };
        let walked = walk(source, m.source, &mut fill);
        let unwound = fill.levels.len();
        let copied = core::mem::take(&mut fill.base.copied);
        // The operand's own level and each the walk was still in.
        for _ in 0..=unwound {
            self.descent.leave();
        }
        walked?;
        let entries = sorted(copied);
        self.carry_attributes((source, m.source), made, m.target)?;
        carry_owner(m.found, made_stat, made, m.target)?;
        Ok(Copied {
            name: concatenated(&[m.source_name]).ok_or(MvError::MemoryExhausted)?,
            kind: Kind::Directory,
            expect: NodeExpect::of_node(m.found.id),
            entries,
        })
    }

    /// Fill the staged file `staged` with the regular file `name` names in
    /// `dir`, opened without following a link — the node `found` describes,
    /// when the look found one: its bytes with its holes, its extended
    /// attributes, then its owner and permission bits. Answers what the
    /// source was as it was copied.
    fn fill_file(
        &mut self,
        (dir, name, path): (&R::Dir, &str, &str),
        found: Option<&FileStat>,
        staged: &mut <R::Dir as Dir>::File,
        target: &str,
    ) -> Result<NodeExpect, MvError> {
        let mut source = dir
            .open_file(name, false)
            .map_err(|errno| read_error(path, errno))?;
        let stat = source.stat().map_err(|errno| read_error(path, errno))?;
        if found.is_some_and(|found| found.id != stat.id) {
            return Err(read_error(path, Errno::Stale));
        }
        if self.buf.is_empty() && !tairix_util::fallible::grow_to(&mut self.buf, CHUNK_LEN, 0) {
            return Err(MvError::MemoryExhausted);
        }
        DataCopy::new(stat.size, Sparse::Auto)
            .run(&mut source, staged, &mut self.buf)
            .map_err(|failure| match failure {
                DataError::Read(errno) => read_error(path, errno),
                DataError::Write(errno) => write_error(target, errno),
            })?;
        self.carry_attributes((&source, path), staged, target)?;
        let made = staged.stat().map_err(|errno| stat_error(target, errno))?;
        carry_owner(&stat, &made, staged, target)?;
        Ok(NodeExpect::of(&stat))
    }

    /// Carry the extended attributes of the held `from`, known as `source`, to
    /// the held `at`, noting what its volume could not keep: the source is
    /// removed next, so nothing it carried is left unsaid.
    fn carry_attributes<S: Node + ?Sized, T: Node + ?Sized>(
        &mut self,
        (from, source): (&S, &str),
        at: &T,
        target: &str,
    ) -> Result<(), MvError> {
        let left = copy_attributes(from, at, Attributes::Carry).map_err(|err| match err {
            AttrError::Read(errno) => read_error(source, errno),
            AttrError::Write { key, error } => MvError::Attribute {
                key,
                target: String::from(target),
                error,
            },
        })?;
        self.omissions.note(&left);
        Ok(())
    }

    /// Put the built copy of the source, held in `stage` under the name
    /// `staged`, in place of the target: on the medium first, then renamed
    /// while its name still holds it; whether it was put there, a destination
    /// that appeared meanwhile being asked about (`-i`) or, under `-n`, kept
    /// and reported.
    fn place(
        &self,
        m: &Move<'_, R::Dir>,
        staged: &str,
        stage: &Stage<R::Dir>,
        flags: RenameFlags,
    ) -> Result<bool, MvError> {
        stage
            .node()
            .sync()
            .map_err(|errno| write_error(m.target, errno))?;
        let expect = NodeExpect::of_node(stage.made.id);
        match self.rename_agreed(
            m.target_dir,
            staged,
            &expect,
            (m.target_dir, m.target_name, m.target),
            flags,
        )? {
            Placement::Kept => Ok(false),
            Placement::Renamed => Ok(true),
            // A staging name shares the destination's directory, so this is
            // a volume answering against itself; nothing is put in place.
            Placement::CrossVolume => Err(MvError::Rename {
                source: String::from(m.source),
                target: String::from(m.target),
                errno: Errno::CrossVolume,
            }),
            Placement::Refused(errno) => Err(refusal(m, errno)),
        }
    }

    /// Report under `-v` the non-directory `source` copied to `target`, as GNU
    /// does; a directory was reported as it was made.
    fn copied(&self, source: &str, kind: Kind, target: &str) -> Result<(), MvError> {
        if kind == Kind::Directory {
            return Ok(());
        }
        self.report(format_args!(
            "copied {} -> {}",
            quote(source),
            quote(target)
        ))
    }

    /// Remove what the move copied, and nothing else: a directory's entries
    /// depth-first through a walk of the held source, a parent only after its
    /// entries, each entry only while it is as it was copied. An entry added
    /// or changed meanwhile is kept and reported, and so is the directory
    /// holding it.
    fn remove_copied(
        &mut self,
        m: &Move<'_, R::Dir>,
        held: Option<&R::Dir>,
        copied: &Copied,
    ) -> Result<(), MvError> {
        let mut base = Kept::default();
        if let Some(source) = held {
            let mut removal = Removal {
                mover: &mut *self,
                levels: alloc::vec![(copied, Kept::default())],
            };
            walk(source, m.source, &mut removal)?;
            if let Some(&(_, kept)) = removal.levels.first() {
                base = kept;
            }
        }
        self.remove_entry(m.source_dir, m.source_name, m.source, copied, base)
            .map(drop)
    }

    /// Remove the copied entry `name` of `dir`, known as `path`, while it is
    /// what `copied` recorded: a directory only while it is that directory and
    /// empty, anything else only while it is unchanged; `inside` is what a
    /// directory's own entries left behind. `-v` reports each removal, as GNU
    /// does; one refused is reported and carried past, and answers what kept
    /// it.
    fn remove_entry(
        &mut self,
        dir: &R::Dir,
        name: &str,
        path: &str,
        copied: &Copied,
        inside: Kept,
    ) -> Result<Kept, MvError> {
        let (flags, removed) = if copied.kind == Kind::Directory {
            (
                UnlinkFlags::DIRECTORY.union(UnlinkFlags::EXPECT_NODE),
                "removed directory",
            )
        } else {
            (UnlinkFlags::EXPECT_UNCHANGED, "removed")
        };
        match dir.unlink(name, flags, Some(&copied.expect)) {
            Ok(()) => self
                .report(format_args!("{removed} {}", quote(path)))
                .map(|()| Kept::default()),
            // Held only by an entry whose refusal was reported: kept unsaid,
            // as GNU keeps an ancestor of what stays.
            Err(Errno::NotEmpty) if !inside.changed && inside.refused => Ok(Kept {
                refused: true,
                ..Kept::default()
            }),
            Err(Errno::Stale | Errno::NotEmpty) => self
                .carried
                .carry(MvError::Changed(String::from(path)))
                .map(|()| Kept {
                    changed: true,
                    ..Kept::default()
                }),
            Err(errno) => self
                .carried
                .carry(remove_error(path, errno))
                .map(|()| Kept {
                    refused: true,
                    ..Kept::default()
                }),
        }
    }

    /// Write one `-v` report line, formatted only when `-v` asks for it.
    fn report(&self, line: core::fmt::Arguments<'_>) -> Result<(), MvError> {
        if !self.options.verbose {
            return Ok(());
        }
        self.out
            .write_all(format!("{line}\n").as_bytes())
            .map_err(MvError::Output)
    }
}

/// One directory a move's fill is in: the staged directory its entries are
/// made in, the source directory it copies, and what it has copied so far.
struct FillLevel<D> {
    /// The directory made and what it was born as; `None` for the staging
    /// directory itself, which the fill borrows.
    made: Option<(D, FileStat)>,
    copied: Vec<Copied>,
}

/// The walk of a source directory that builds its copy, entry by entry,
/// through the staged directories it holds.
struct Fill<'f, 'a, R: Roots> {
    mover: &'f mut Mover<'a, R>,
    /// The staging directory, which the fill borrows.
    root: &'f R::Dir,
    /// The operand directory's own level, whose entries go in `root`.
    base: FillLevel<R::Dir>,
    /// Each level below it, holding its staged directory.
    levels: Vec<FillLevel<R::Dir>>,
    /// How long the source's own path is, and the destination it moves to:
    /// what a path beneath the source is called at the destination.
    spelled: (usize, &'f str),
}

impl<R: Roots> Fill<'_, '_, R> {
    /// Where the source's `path` is copied to, as the user knows the
    /// destination.
    fn target_of(&self, path: &str) -> String {
        let (source, target) = self.spelled;
        join(
            target,
            path.get(source..).unwrap_or("").trim_start_matches('/'),
        )
    }

    fn top_mut(&mut self) -> &mut FillLevel<R::Dir> {
        match self.levels.last_mut() {
            Some(level) => level,
            None => &mut self.base,
        }
    }

    /// Record a copied entry in the directory being filled.
    fn record(&mut self, copied: Copied) -> Result<(), MvError> {
        let level = self.top_mut();
        level
            .copied
            .try_reserve(1)
            .map_err(|_| MvError::MemoryExhausted)?;
        level.copied.push(copied);
        Ok(())
    }
}

impl<R: Roots> Visitor<R::Dir> for Fill<'_, '_, R> {
    type Error = MvError;

    fn enter(&mut self, dir: &R::Dir, entry: &Entry, path: &str) -> Result<Step<R::Dir>, MvError> {
        // Each field borrowed apart, so the mover may be lent on.
        let level = self.levels.last().unwrap_or(&self.base);
        let into = level.made.as_ref().map_or(self.root, |(made, _)| made);
        let target = self.target_of(path);
        match entry.kind {
            Kind::Directory => {
                // Held from here, so the walk goes on beneath this very node.
                let held = dir
                    .open_dir(&entry.name)
                    .map_err(|errno| read_error(path, errno))?;
                let source = held.stat().map_err(|errno| stat_error(path, errno))?;
                if self.mover.descent.refuses(source.id) {
                    return Err(MvError::IntoItself {
                        source: String::from(path),
                        target,
                    });
                }
                let made = into
                    .make_dir(&entry.name, OWNER_RWX)
                    .map_err(|errno| create_error(&target, errno))?;
                let (born, _) = DirModes::new(OWNER_RWX)
                    .ready(&made)
                    .map_err(|errno| create_error(&target, errno))?;
                self.levels
                    .try_reserve(1)
                    .map_err(|_| MvError::MemoryExhausted)?;
                self.mover
                    .report(format_args!("created directory {}", quote(&target)))?;
                self.mover
                    .descent
                    .enter(source.id, born.id)
                    .map_err(|_| MvError::MemoryExhausted)?;
                self.levels.push(FillLevel {
                    made: Some((made, born)),
                    copied: Vec::new(),
                });
                Ok(Step::Into(held))
            }
            Kind::File => {
                let mut staged = into
                    .create_file(&entry.name, OWNER_RW)
                    .map_err(|errno| create_error(&target, errno))?;
                let expect =
                    self.mover
                        .fill_file((dir, &entry.name, path), None, &mut staged, &target)?;
                self.mover.copied(path, Kind::File, &target)?;
                self.record(leaf(&entry.name, Kind::File, expect)?)?;
                Ok(Step::Skip)
            }
            Kind::Link => {
                let source = dir
                    .open_node(&entry.name)
                    .and_then(|node| node.stat())
                    .map_err(|errno| read_error(path, errno))?;
                let stored = dir
                    .readlink(&entry.name)
                    .map_err(|errno| read_error(path, errno))?;
                into.symlink(stored.as_bytes(), &entry.name)
                    .map_err(|errno| create_error(&target, errno))?;
                // Reached by its name again: a name another could swap in a
                // shared directory, where it could already make one itself.
                let made = into
                    .open_node(&entry.name)
                    .and_then(|made| made.stat().map(|stat| (made, stat)))
                    .map_err(|errno| create_error(&target, errno))?;
                if made.1.kind == FileKind::Symlink {
                    carry_owner(&source, &made.1, &made.0, &target)?;
                }
                self.mover.copied(path, Kind::Link, &target)?;
                self.record(leaf(&entry.name, Kind::Link, NodeExpect::of(&source))?)?;
                Ok(Step::Skip)
            }
        }
    }

    fn leave(
        &mut self,
        _: &R::Dir,
        entry: &Entry,
        path: &str,
        held: &R::Dir,
    ) -> Result<(), MvError> {
        let Some(level) = self.levels.pop() else {
            return Ok(());
        };
        self.mover.descent.leave();
        let source = held.stat().map_err(|errno| read_error(path, errno))?;
        let Some((made, born)) = level.made else {
            return Ok(());
        };
        let target = self.target_of(path);
        self.mover.carry_attributes((held, path), &made, &target)?;
        carry_owner(&source, &born, &made, &target)?;
        self.record(Copied {
            name: concatenated(&[&entry.name]).ok_or(MvError::MemoryExhausted)?,
            kind: Kind::Directory,
            expect: NodeExpect::of_node(source.id),
            entries: sorted(level.copied),
        })
    }

    fn refused(&mut self, _: &R::Dir, _: &Entry, path: &str, errno: Errno) -> Result<(), MvError> {
        if self.levels.pop().is_some() {
            self.mover.descent.leave();
        }
        Err(read_error(path, errno))
    }

    fn unlisted_root(&mut self, path: &str, errno: Errno) -> Result<(), MvError> {
        Err(read_error(path, errno))
    }
}

/// The walk of a moved source directory that removes what was copied of it.
struct Removal<'f, 'a, 'r, R: Roots> {
    mover: &'f mut Mover<'a, R>,
    /// The record of each directory the walk is in, and what it kept so far.
    levels: Vec<(&'r Copied, Kept)>,
}

/// What kept entries of a directory a move removes what it copied of.
#[derive(Clone, Copy, Default)]
struct Kept {
    /// One was added or changed since it was copied.
    changed: bool,
    /// One could not be removed, for a reason already reported.
    refused: bool,
}

impl Kept {
    const fn and(self, other: Self) -> Self {
        Self {
            changed: self.changed || other.changed,
            refused: self.refused || other.refused,
        }
    }
}

impl<R: Roots> Removal<'_, '_, '_, R> {
    fn note(&mut self, kept: Kept) {
        if let Some((_, level)) = self.levels.last_mut() {
            *level = level.and(kept);
        }
    }
}

impl<R: Roots> Visitor<R::Dir> for Removal<'_, '_, '_, R> {
    type Error = MvError;

    fn enter(&mut self, dir: &R::Dir, entry: &Entry, path: &str) -> Result<Step<R::Dir>, MvError> {
        let Some(&(level, _)) = self.levels.last() else {
            return Ok(Step::Skip);
        };
        // An entry the copy never saw was added since: it is kept, and the
        // directory holding it with it.
        let Some(copied) = level.entry(&entry.name) else {
            self.note(Kept {
                changed: true,
                ..Kept::default()
            });
            return Ok(Step::Skip);
        };
        if copied.kind == Kind::Directory && entry.kind == Kind::Directory {
            self.levels
                .try_reserve(1)
                .map_err(|_| MvError::MemoryExhausted)?;
            self.levels.push((copied, Kept::default()));
            return Ok(Step::Descend);
        }
        let kept = self
            .mover
            .remove_entry(dir, &entry.name, path, copied, Kept::default())?;
        self.note(kept);
        Ok(Step::Skip)
    }

    fn leave(
        &mut self,
        dir: &R::Dir,
        entry: &Entry,
        path: &str,
        _: &R::Dir,
    ) -> Result<(), MvError> {
        let Some((copied, inside)) = self.levels.pop() else {
            return Ok(());
        };
        let kept = self
            .mover
            .remove_entry(dir, &entry.name, path, copied, inside)?;
        self.note(kept);
        Ok(())
    }

    fn refused(&mut self, _: &R::Dir, _: &Entry, path: &str, errno: Errno) -> Result<(), MvError> {
        self.levels.pop();
        self.mover.carried.carry(remove_error(path, errno))?;
        self.note(Kept {
            refused: true,
            ..Kept::default()
        });
        Ok(())
    }

    fn unlisted_root(&mut self, path: &str, errno: Errno) -> Result<(), MvError> {
        self.mover.carried.carry(remove_error(path, errno))?;
        self.note(Kept {
            refused: true,
            ..Kept::default()
        });
        Ok(())
    }
}

/// What a refused move is called, in GNU's terms where the rename's rule
/// was broken.
fn refusal<D: Dir>(m: &Move<'_, D>, errno: Errno) -> MvError {
    let occupant = m
        .target_dir
        .open_node(m.target_name)
        .and_then(|node| node.stat())
        .ok()
        .map(|stat| Kind::of(stat.kind));
    let directory = m.kind == Kind::Directory;
    match (errno, occupant) {
        (Errno::IsADirectory, Some(Kind::Directory)) => MvError::OverwriteDirectory {
            target: String::from(m.target),
        },
        (Errno::NotADirectory, Some(occupant)) if directory && occupant != Kind::Directory => {
            MvError::OverwriteNonDirectory {
                source: String::from(m.source),
                target: String::from(m.target),
            }
        }
        (Errno::OutOfRange, _)
            if directory && lands_inside(m.source_dir, m.source_name, m.target_dir) =>
        {
            MvError::IntoItself {
                source: String::from(m.source),
                target: String::from(m.target),
            }
        }
        _ => MvError::Rename {
            source: String::from(m.source),
            target: String::from(m.target),
            errno,
        },
    }
}

/// The directory source, opened as a directory and never through a link:
/// the very node the look found.
fn held_source<D: Dir>(m: &Move<'_, D>) -> Result<D, MvError> {
    let held = m
        .source_dir
        .open_dir(m.source_name)
        .map_err(|errno| read_error(m.source, errno))?;
    let id = held.stat().map_err(|errno| read_error(m.source, errno))?.id;
    if id == m.found.id {
        Ok(held)
    } else {
        Err(read_error(m.source, Errno::Stale))
    }
}

/// Refuse, before anything is copied, a move across volumes the rename
/// itself would refuse: a directory into its own subtree, a directory onto
/// anything but an empty directory, anything else onto a directory.
fn admit<D: Dir>(m: &Move<'_, D>, held: Option<&D>) -> Result<(), MvError> {
    if let Some(source) = held {
        let landed = landing(source, m.target_dir).map_err(|unresolved| {
            let (path, errno) = unresolved.at(m.source, m.target);
            stat_error(path, errno)
        })?;
        if landed != Landing::Apart {
            return Err(MvError::IntoItself {
                source: String::from(m.source),
                target: String::from(m.target),
            });
        }
    }
    let occupant = absent_as_none(
        m.target,
        m.target_dir
            .open_node(m.target_name)
            .and_then(|node| node.stat()),
    )?
    .map(|stat| Kind::of(stat.kind));
    let Some(occupant) = occupant else {
        return Ok(());
    };
    let directory = m.kind == Kind::Directory;
    match occupant {
        Kind::Directory if directory => {
            let empty = m
                .target_dir
                .open_dir(m.target_name)
                .and_then(|dir| dir.entries())
                .map_err(|errno| read_error(m.target, errno))?
                .is_empty();
            if empty {
                Ok(())
            } else {
                Err(MvError::Rename {
                    source: String::from(m.source),
                    target: String::from(m.target),
                    errno: Errno::NotEmpty,
                })
            }
        }
        Kind::Directory => Err(MvError::OverwriteDirectory {
            target: String::from(m.target),
        }),
        _ if directory => Err(MvError::OverwriteNonDirectory {
            source: String::from(m.source),
            target: String::from(m.target),
        }),
        _ => Ok(()),
    }
}

/// Give the held `at`, read as `current`, the owner and permission bits
/// `wanted` describes, as the shared engine's move does, a refusal naming
/// `target`.
fn carry_owner<T: Node + ?Sized>(
    wanted: &FileStat,
    current: &FileStat,
    at: &T,
    target: &str,
) -> Result<(), MvError> {
    tairix_fscopy::carry_owner(Ownership::of(wanted), Ownership::of(current), at)
        .map_err(|errno| write_error(target, errno))
}

/// Remove the staging name `staged` the mover just made in `dir`, before it
/// could read what the name holds: by the name alone, as far as it will go.
fn forsake<D: Dir>(dir: &D, staged: &str) {
    let _ = dir.unlink(staged, UnlinkFlags::empty(), None);
}

/// Remove a staged copy that was not put in place, as far as it will go,
/// through the node it was built in — never by a name, which another may have
/// swapped since — and its staging name only while that name still holds it.
fn discard<D: Dir>(dir: &D, staged: &str, stage: &Stage<D>) {
    let flags = match &stage.held {
        Staged::Directory(made) => {
            // It is the mover's own, so one built without write permission
            // is opened first.
            let _ = made.set_mode(OWNER_RWX);
            let mut emptier = Emptier {
                opened: alloc::vec![true],
            };
            let _ = walk(made, "", &mut emptier);
            UnlinkFlags::DIRECTORY.union(UnlinkFlags::EXPECT_NODE)
        }
        Staged::File(_) | Staged::Link(_) => UnlinkFlags::EXPECT_NODE,
    };
    let _ = dir.unlink(staged, flags, Some(&NodeExpect::of_node(stage.made.id)));
}

/// The walk that empties a staged directory the mover made, opening each
/// directory to its owner as it reaches it.
struct Emptier {
    /// Whether each directory the walk is in was opened to its owner yet.
    opened: Vec<bool>,
}

impl<D: Dir> Visitor<D> for Emptier {
    type Error = core::convert::Infallible;

    fn enter(&mut self, dir: &D, entry: &Entry, _: &str) -> Result<Step<D>, Self::Error> {
        if let Some(opened) = self.opened.last_mut() {
            if !*opened {
                let _ = dir.set_mode(OWNER_RWX);
                *opened = true;
            }
        }
        if entry.kind == Kind::Directory {
            self.opened.push(false);
            return Ok(Step::Descend);
        }
        let _ = dir.unlink(&entry.name, UnlinkFlags::empty(), None);
        Ok(Step::Skip)
    }

    fn leave(&mut self, dir: &D, entry: &Entry, _: &str, _: &D) -> Result<(), Self::Error> {
        self.opened.pop();
        let _ = dir.unlink(&entry.name, UnlinkFlags::DIRECTORY, None);
        Ok(())
    }

    fn refused(&mut self, _: &D, _: &Entry, _: &str, _: Errno) -> Result<(), Self::Error> {
        self.opened.pop();
        Ok(())
    }

    fn unlisted_root(&mut self, _: &str, _: Errno) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// The record of a copied non-directory.
/// The record of the entry `name`, copied as `kind` from what `expect`
/// describes; a record the heap cannot hold ends the move before anything is
/// removed.
fn leaf(name: &str, kind: Kind, expect: NodeExpect) -> Result<Copied, MvError> {
    Ok(Copied {
        name: concatenated(&[name]).ok_or(MvError::MemoryExhausted)?,
        kind,
        expect,
        entries: Vec::new(),
    })
}

/// `entries` in name order, so a removal finds each by halving.
fn sorted(mut entries: Vec<Copied>) -> Vec<Copied> {
    entries.sort_unstable_by(|a, b| a.name.cmp(&b.name));
    entries
}

fn absent_as_none(path: &str, probe: Result<FileStat, Errno>) -> Result<Option<FileStat>, MvError> {
    match probe {
        Ok(stat) => Ok(Some(stat)),
        Err(Errno::NotFound) => Ok(None),
        Err(errno) => Err(stat_error(path, errno)),
    }
}

/// The destination `path` refused as `mv`'s target, `directory` when `-t`
/// named it.
fn target_error(path: &str, directory: bool, errno: Errno) -> MvError {
    MvError::Target {
        path: String::from(path),
        directory,
        errno,
    }
}

fn stat_error(path: &str, errno: Errno) -> MvError {
    MvError::Stat {
        path: String::from(path),
        errno,
    }
}

fn read_error(path: &str, errno: Errno) -> MvError {
    MvError::Read {
        path: String::from(path),
        errno,
    }
}

fn create_error(path: &str, errno: Errno) -> MvError {
    MvError::Create {
        path: String::from(path),
        errno,
    }
}

fn write_error(path: &str, errno: Errno) -> MvError {
    MvError::Write {
        path: String::from(path),
        errno,
    }
}

fn remove_error(path: &str, errno: Errno) -> MvError {
    MvError::Remove {
        path: String::from(path),
        errno,
    }
}

#[cfg(test)]
mod tests;
