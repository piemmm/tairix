//! The move engine: resolve each source's destination and rename it there.
//! A rename that would cross a filesystem boundary instead builds the
//! source's copy under a staging name beside the destination, puts it on the
//! medium, renames it into place, and only then removes the source — so the
//! destination always names either what it held or the whole source, and no
//! failure or crash loses either.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::fs::{FS_MODE_MASK, FS_SETGID_BIT, FS_SETUID_BIT};
use tairix_abi::{Errno, FileId, RenameFlags};
use tairix_fscopy::{
    copy_attributes, landing, AttrError, Attributes, Created, DataCopy, DataError, Descent,
    Landing, Omissions, PathFile, Sparse, CHUNK_LEN,
};
use tairix_hash::BuildSipHash13;
use tairix_help::{own_short_help, HelpSource};
use tairix_path::{join, leaf_name};
use tairix_util::quote::quote;

use crate::command::{Clobber, Command, Options, TargetMode};
use crate::error::MvError;
use crate::io::{Entry, EntryKind, FileSystem, Output, Prompt, RenameOutcome};

/// The usage banner a usage error is reported with, and the fallback the
/// short-help switches print when `mv`'s own Help tree is unavailable.
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
puts it on the disk, renames it into place, and only then removes the source,
so the destination holds what it held until the move is whole. Attributes the
destination cannot keep are reported in one warning at the end.
";

/// `mv`'s own command word: the short-help switches render its own Help
/// document through the same engine as any other command's.
const OWN_WORD: &str = "mv";

/// What a run reaches outside itself through.
#[derive(Clone, Copy)]
pub struct Seams<'a> {
    /// The files moved.
    pub fs: &'a dyn FileSystem,
    /// The `-i` question.
    pub prompt: &'a dyn Prompt,
    /// The tool's own `Help/` tree, read by the short-help switches.
    pub help: &'a dyn HelpSource,
    /// The `-v` reports, the usage banner and the information stream.
    pub out: &'a dyn Output,
    /// The end-of-run warning about attributes left behind.
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
/// The first failure stops the run before any later operand (fail closed).
///
/// # Errors
///
/// * [`MvError::Usage`] — more than one source aimed at a non-directory
///   destination (or at any destination under `-T`), or a `-t` operand
///   that is not a directory.
/// * [`MvError::Stat`] — an operand could not be inspected.
/// * [`MvError::Rename`] — the move was refused.
/// * [`MvError::IntoItself`], [`MvError::OverwriteDirectory`],
///   [`MvError::OverwriteNonDirectory`] — the move breaks the rule a rename
///   keeps.
/// * [`MvError::JustCreated`] — an earlier operand put the destination
///   there.
/// * [`MvError::Read`] / [`MvError::Create`] / [`MvError::Write`] /
///   [`MvError::Attribute`] — a move across volumes failed before the
///   destination was replaced; the source is kept.
/// * [`MvError::Remove`] — the source could not be removed after its move.
/// * [`MvError::Prompt`] — a confirmation could not be read (never treated
///   as consent).
/// * [`MvError::Output`] — writing the usage banner or a `-v` report
///   failed.
///
/// Attributes a destination could not keep are reported once, after the
/// last move and before any failure that ended the run: a warning on the
/// diagnostics stream and an advisory record on the information stream.
pub fn run(command: Command, locale: Option<&str>, seams: Seams<'_>) -> Result<(), MvError> {
    match command {
        Command::Help => short_help(locale, seams.help, seams.out),
        Command::Move {
            options,
            sources,
            dest,
        } => {
            let mut mover = Mover {
                options,
                fs: seams.fs,
                prompt: seams.prompt,
                out: seams.out,
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
    /// It was carried out, within the volume or not.
    Done(RenameOutcome),
    /// The destination that appeared since the look was kept, as `-n` or a
    /// declined `-i` question says.
    Kept,
    /// The kernel refused it.
    Refused(Errno),
}

/// One `mv` run: its options, its seams, the walk of a tree moving across,
/// and what those moves' destinations could not keep.
struct Mover<'a> {
    options: Options,
    fs: &'a dyn FileSystem,
    prompt: &'a dyn Prompt,
    out: &'a dyn Output,
    /// What the destinations across volumes could not keep, reported once.
    omissions: Omissions,
    /// The directories the walk is inside, so it never enters one again.
    descent: Descent,
    /// The destinations earlier operands put in place.
    created: Created,
    /// The one buffer every relocated file's bytes pass through.
    buf: Vec<u8>,
}

impl Mover<'_> {
    /// Move every source to `dest`, deciding per source whether the
    /// destination is `dest` itself or a child of `dest` named after the
    /// source. A destination is probed through a final link: a directory
    /// reached by one is still the directory the moves land in.
    fn move_all(&mut self, sources: &[String], dest: &str) -> Result<(), MvError> {
        let dest_is_dir = match self.options.target_mode {
            // `-t`: the destination must be an existing directory.
            TargetMode::Directory => match resolved(dest, self.fs)? {
                Some(EntryKind::Directory) => true,
                Some(EntryKind::File | EntryKind::Symlink) => return Err(MvError::Usage),
                None => return Err(stat_error(dest, Errno::NotFound)),
            },
            // `-T`: the destination is a normal file for exactly one source.
            TargetMode::NoDirectory => {
                if sources.len() > 1 {
                    return Err(MvError::Usage);
                }
                false
            }
            TargetMode::Inferred => matches!(resolved(dest, self.fs)?, Some(EntryKind::Directory)),
        };
        // More than one source can only land inside a directory.
        if sources.len() > 1 && !dest_is_dir {
            return Err(MvError::Usage);
        }
        for source in sources {
            let target = if dest_is_dir {
                join(dest, leaf_name(source))
            } else {
                String::from(dest)
            };
            self.move_operand(source, &target)?;
        }
        Ok(())
    }

    /// Move one named source operand, a link as itself, to `target`.
    fn move_operand(&mut self, source: &str, target: &str) -> Result<(), MvError> {
        let kind = self
            .fs
            .kind(source)
            .map_err(|errno| stat_error(source, errno))?;
        let occupied = own(target, self.fs)?.is_some();
        let guarded = kind != EntryKind::Directory;
        if guarded && occupied && self.created.holds(self.identity(target)?, target) {
            return Err(MvError::JustCreated {
                source: String::from(source),
                target: String::from(target),
            });
        }
        if occupied && !self.may_replace(target)? {
            return Ok(());
        }
        // Under `-n` or `-i` nothing the look did not decide about may be
        // replaced: a destination created since is decided as if it had been
        // there, with the kernel refusing the move rather than destroying it.
        let flags = if occupied || self.options.clobber == Clobber::Overwrite {
            RenameFlags::empty()
        } else {
            RenameFlags::NO_REPLACE
        };
        let placed = match self.rename_agreed(source, target, flags)? {
            Placement::Kept => return Ok(()),
            Placement::Done(RenameOutcome::Renamed) => {
                self.report(&format!("renamed {} -> {}\n", quote(source), quote(target)))?;
                true
            }
            Placement::Done(RenameOutcome::CrossDevice) => {
                self.relocate(source, kind, target, flags)?
            }
            Placement::Refused(errno) => return Err(self.refusal(source, kind, target, errno)),
        };
        if guarded && placed {
            let id = self.identity(target)?;
            self.created
                .note(id, target)
                .map_err(|errno| write_error(target, errno))?;
        }
        Ok(())
    }

    /// The identity of the node `target` names itself.
    fn identity(&self, target: &str) -> Result<FileId, MvError> {
        self.fs
            .identity(target)
            .map_err(|errno| stat_error(target, errno))
    }

    /// Rename `from` onto `target` under `flags`, asking again about a
    /// destination that appeared since the look.
    fn rename_agreed(
        &self,
        from: &str,
        target: &str,
        flags: RenameFlags,
    ) -> Result<Placement, MvError> {
        let renamed = match self.fs.rename(from, target, flags) {
            Err(Errno::AlreadyExists) if flags.refuses_replace() => {
                if !self.may_replace(target)? {
                    return Ok(Placement::Kept);
                }
                self.fs.rename(from, target, RenameFlags::empty())
            }
            other => other,
        };
        Ok(match renamed {
            Ok(outcome) => Placement::Done(outcome),
            Err(errno) => Placement::Refused(errno),
        })
    }

    /// Whether the existing `target` may be replaced: by default and under
    /// `-f` always, never under `-n`, and on the user's answer under `-i`.
    fn may_replace(&self, target: &str) -> Result<bool, MvError> {
        match self.options.clobber {
            Clobber::Overwrite => Ok(true),
            Clobber::Skip => Ok(false),
            Clobber::Prompt => self
                .prompt
                .confirm(&format!("overwrite {}?", quote(target)))
                .map_err(MvError::Prompt),
        }
    }

    /// What a refused move of `source` onto `target` is called, in GNU's
    /// terms where the rename's rule was broken.
    fn refusal(&self, source: &str, kind: EntryKind, target: &str, errno: Errno) -> MvError {
        let occupant = own(target, self.fs).ok().flatten();
        match (errno, occupant) {
            (Errno::IsADirectory, Some(EntryKind::Directory)) => MvError::OverwriteDirectory {
                target: String::from(target),
            },
            (Errno::NotADirectory, Some(EntryKind::File | EntryKind::Symlink))
                if kind == EntryKind::Directory =>
            {
                MvError::OverwriteNonDirectory {
                    source: String::from(source),
                    target: String::from(target),
                }
            }
            (Errno::OutOfRange, _)
                if kind == EntryKind::Directory && self.lands_in(source, target) =>
            {
                MvError::IntoItself {
                    source: String::from(source),
                    target: String::from(target),
                }
            }
            _ => MvError::Rename {
                source: String::from(source),
                target: String::from(target),
                errno,
            },
        }
    }

    /// Whether `target` is the directory `source` or lies inside it.
    fn lands_in(&self, source: &str, target: &str) -> bool {
        self.fs
            .identity(source)
            .and_then(|id| landing(self.fs, source, id, target))
            .is_ok_and(|landed| landed != Landing::Apart)
    }

    /// Move `source` across a filesystem boundary onto `target`, replacing it
    /// in one rename once the copy is whole and on the medium; `flags` is the
    /// replacement the user agreed to. Whether it was put there, or kept out
    /// by a destination that appeared meanwhile.
    fn relocate(
        &mut self,
        source: &str,
        kind: EntryKind,
        target: &str,
        flags: RenameFlags,
    ) -> Result<bool, MvError> {
        self.admit(source, kind, target)?;
        let staged = self
            .fs
            .staging_name(target)
            .map_err(|errno| create_error(target, errno))?;
        // A staging name another holds is refused here, before there is
        // anything of the mover's to discard.
        self.make(source, kind, &staged, target)?;
        let placed = self
            .fill(source, kind, &staged, target)
            .and_then(|()| self.place(source, kind, &staged, target, flags));
        match placed {
            Ok(true) => {}
            Ok(false) => {
                discard(&staged, kind, self.fs);
                return Ok(false);
            }
            Err(err) => {
                discard(&staged, kind, self.fs);
                return Err(err);
            }
        }
        // The source goes only once its new name is on the medium too.
        self.fs
            .sync(target)
            .map_err(|errno| write_error(target, errno))?;
        self.report(&format!("copied {} -> {}\n", quote(source), quote(target)))?;
        remove_tree(source, kind, self.fs)?;
        self.report(&format!("removed {}\n", quote(source)))?;
        Ok(true)
    }

    /// Refuse, before anything is copied, a move across volumes the rename
    /// itself would refuse: a directory into its own subtree, a directory onto
    /// anything but an empty directory, anything else onto a directory.
    fn admit(&self, source: &str, kind: EntryKind, target: &str) -> Result<(), MvError> {
        let named = |errno| MvError::Rename {
            source: String::from(source),
            target: String::from(target),
            errno,
        };
        if kind == EntryKind::Directory {
            let id = self
                .fs
                .identity(source)
                .map_err(|errno| stat_error(source, errno))?;
            let landed =
                landing(self.fs, source, id, target).map_err(|errno| stat_error(target, errno))?;
            if landed != Landing::Apart {
                return Err(MvError::IntoItself {
                    source: String::from(source),
                    target: String::from(target),
                });
            }
        }
        match (kind, own(target, self.fs)?) {
            (_, None)
            | (EntryKind::File | EntryKind::Symlink, Some(EntryKind::File | EntryKind::Symlink)) => {
                Ok(())
            }
            (EntryKind::Directory, Some(EntryKind::Directory)) => {
                match self.fs.read_dir(target, 0) {
                    Ok(None) => Ok(()),
                    Ok(Some(_)) => Err(named(Errno::NotEmpty)),
                    Err(errno) => Err(read_error(target, errno)),
                }
            }
            (EntryKind::File | EntryKind::Symlink, Some(EntryKind::Directory)) => {
                Err(MvError::OverwriteDirectory {
                    target: String::from(target),
                })
            }
            (EntryKind::Directory, Some(EntryKind::File | EntryKind::Symlink)) => {
                Err(MvError::OverwriteNonDirectory {
                    source: String::from(source),
                    target: String::from(target),
                })
            }
        }
    }

    /// Build `source`'s copy at the fresh name `at`, which will replace
    /// `target`: failures name `target`'s own path, never the staging one.
    fn build(
        &mut self,
        source: &str,
        kind: EntryKind,
        at: &str,
        target: &str,
    ) -> Result<(), MvError> {
        self.make(source, kind, at, target)?;
        self.fill(source, kind, at, target)
    }

    /// Make the node `at` that `source`'s copy is built in, refusing a name
    /// that is already taken.
    fn make(&self, source: &str, kind: EntryKind, at: &str, target: &str) -> Result<(), MvError> {
        let made = match kind {
            EntryKind::File => self.fs.create(at),
            EntryKind::Directory => self.fs.mkdir(at),
            EntryKind::Symlink => {
                let stored = self
                    .fs
                    .read_link(source)
                    .map_err(|errno| read_error(source, errno))?;
                self.fs.symlink(&stored, at)
            }
        };
        made.map_err(|errno| create_error(target, errno))
    }

    /// Fill the node [`make`](Self::make) made at `at` with what `source`
    /// holds; a link holds nothing more.
    fn fill(
        &mut self,
        source: &str,
        kind: EntryKind,
        at: &str,
        target: &str,
    ) -> Result<(), MvError> {
        match kind {
            EntryKind::File => self.fill_file(source, at, target),
            EntryKind::Directory => self.fill_directory(source, at, target),
            EntryKind::Symlink => Ok(()),
        }
    }

    /// Fill the directory `at` with `source`'s entries, attributes, owner and
    /// permission bits; the bits last, so a directory its owner may not write
    /// still takes its entries.
    fn fill_directory(&mut self, source: &str, at: &str, target: &str) -> Result<(), MvError> {
        let source_id = self
            .fs
            .identity(source)
            .map_err(|errno| stat_error(source, errno))?;
        let made_id = self
            .fs
            .identity(at)
            .map_err(|errno| stat_error(target, errno))?;
        self.descent
            .enter(source_id, made_id)
            .map_err(|errno| write_error(target, errno))?;
        for entry in read_children(source, self.fs)? {
            let child = join(source, &entry.name);
            let child_target = join(target, &entry.name);
            if entry.kind == EntryKind::Directory {
                let id = self
                    .fs
                    .identity(&child)
                    .map_err(|errno| stat_error(&child, errno))?;
                if self.descent.refuses(id) {
                    return Err(MvError::IntoItself {
                        source: child,
                        target: child_target,
                    });
                }
            }
            self.build(&child, entry.kind, &join(at, &entry.name), &child_target)?;
        }
        self.descent.leave();
        self.carry_attributes(source, at, target)?;
        self.carry_owner(source, at, target)
    }

    /// Fill the file `at` with the regular file `source`: its bytes with its
    /// holes, its extended attributes, then its owner and permission bits.
    fn fill_file(&mut self, source: &str, at: &str, target: &str) -> Result<(), MvError> {
        let len = self
            .fs
            .size(source)
            .map_err(|errno| read_error(source, errno))?;
        if self.buf.is_empty() && !tairix_util::fallible::grow_to(&mut self.buf, CHUNK_LEN, 0) {
            return Err(write_error(target, Errno::OutOfMemory));
        }
        DataCopy::new(len, Sparse::Auto)
            .run(
                &mut PathFile::new(self.fs, source),
                &mut PathFile::new(self.fs, at),
                &mut self.buf,
            )
            .map_err(|failure| match failure {
                DataError::Read(errno) => read_error(source, errno),
                DataError::Write(errno) => write_error(target, errno),
            })?;
        self.carry_attributes(source, at, target)?;
        self.carry_owner(source, at, target)
    }

    /// Carry `source`'s extended attributes to `at`, noting what its volume
    /// could not keep: the source is removed next, so nothing it carried is
    /// left unsaid.
    fn carry_attributes(&mut self, source: &str, at: &str, target: &str) -> Result<(), MvError> {
        let left =
            copy_attributes(self.fs, source, at, Attributes::Carry).map_err(|err| match err {
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

    /// Give `at` the owner and permission bits of `source` as far as its
    /// volume keeps them and the mover may give them. A node the mover may
    /// not give away stays the mover's, without its set-ID bits, so a move
    /// never grants the mover a privilege the source held for someone else.
    fn carry_owner(&self, source: &str, at: &str, target: &str) -> Result<(), MvError> {
        let wanted = self
            .fs
            .owner(source)
            .map_err(|errno| stat_error(source, errno))?;
        let current = self
            .fs
            .owner(at)
            .map_err(|errno| stat_error(target, errno))?;
        let mut mode = wanted.mode & FS_MODE_MASK;
        if (current.uid, current.gid) != (wanted.uid, wanted.gid) {
            match self.fs.set_owner(at, wanted.uid, wanted.gid) {
                Ok(()) => {}
                Err(Errno::PermissionDenied | Errno::NotSupported) => {
                    mode &= !(FS_SETUID_BIT | FS_SETGID_BIT);
                }
                Err(errno) => return Err(write_error(target, errno)),
            }
        }
        match self.fs.set_mode(at, mode) {
            Ok(()) | Err(Errno::NotSupported) => Ok(()),
            Err(errno) => Err(write_error(target, errno)),
        }
    }

    /// Put the built `staged` copy of `source` in place of `target`: on the
    /// medium first, then renamed; whether it was put there, a destination
    /// that appeared meanwhile being asked about (`-i`) or kept (`-n`).
    fn place(
        &self,
        source: &str,
        kind: EntryKind,
        staged: &str,
        target: &str,
        flags: RenameFlags,
    ) -> Result<bool, MvError> {
        self.fs
            .sync(staged)
            .map_err(|errno| write_error(target, errno))?;
        match self.rename_agreed(staged, target, flags)? {
            Placement::Kept => Ok(false),
            Placement::Done(RenameOutcome::Renamed) => Ok(true),
            // A staging name shares the destination's directory, so this is
            // a volume answering against itself; nothing is put in place.
            Placement::Done(RenameOutcome::CrossDevice) => Err(MvError::Rename {
                source: String::from(source),
                target: String::from(target),
                errno: Errno::CrossVolume,
            }),
            Placement::Refused(errno) => Err(self.refusal(source, kind, target, errno)),
        }
    }

    /// Write one `-v` report.
    fn report(&self, line: &str) -> Result<(), MvError> {
        if !self.options.verbose {
            return Ok(());
        }
        self.out.write_all(line.as_bytes()).map_err(MvError::Output)
    }
}

/// Remove an already-moved object whose [`EntryKind`] is known,
/// depth-first for a directory so a parent is removed only after its
/// contents; a link is unlinked itself, never followed.
fn remove_tree(source: &str, kind: EntryKind, fs: &dyn FileSystem) -> Result<(), MvError> {
    let removed = |result: Result<(), Errno>| result.map_err(|errno| remove_error(source, errno));
    match kind {
        EntryKind::File | EntryKind::Symlink => removed(fs.remove_file(source)),
        EntryKind::Directory => {
            for entry in read_children(source, fs)? {
                remove_tree(&join(source, &entry.name), entry.kind, fs)?;
            }
            removed(fs.remove_dir(source))
        }
    }
}

/// Remove a staged copy that was not put in place, as far as it will go: it
/// is the mover's own, so a directory it built without write permission is
/// opened first.
fn discard(staged: &str, kind: EntryKind, fs: &dyn FileSystem) {
    if kind == EntryKind::Directory {
        let _ = fs.set_mode(staged, 0o700);
        if let Ok(children) = read_children(staged, fs) {
            for entry in children {
                discard(&join(staged, &entry.name), entry.kind, fs);
            }
        }
        let _ = fs.remove_dir(staged);
    } else {
        let _ = fs.remove_file(staged);
    }
}

/// Read every entry of `path` into a vector, so the directory can be walked
/// without depending on entry indices staying stable as the move proceeds.
fn read_children(path: &str, fs: &dyn FileSystem) -> Result<Vec<Entry>, MvError> {
    let mut entries = Vec::new();
    let mut index: u64 = 0;
    while let Some(entry) = fs
        .read_dir(path, index)
        .map_err(|errno| read_error(path, errno))?
    {
        index = index.saturating_add(1);
        entries.push(entry);
    }
    Ok(entries)
}

/// What `path` names itself, a missing path [`None`], so a destination can be
/// probed for existence without treating absence as a failure.
fn own(path: &str, fs: &dyn FileSystem) -> Result<Option<EntryKind>, MvError> {
    absent_as_none(path, fs.kind(path))
}

/// What `path` resolves to, a final link followed; a missing path [`None`].
fn resolved(path: &str, fs: &dyn FileSystem) -> Result<Option<EntryKind>, MvError> {
    absent_as_none(path, fs.resolved_kind(path))
}

fn absent_as_none(
    path: &str,
    probe: Result<EntryKind, Errno>,
) -> Result<Option<EntryKind>, MvError> {
    match probe {
        Ok(kind) => Ok(Some(kind)),
        Err(Errno::NotFound) => Ok(None),
        Err(errno) => Err(stat_error(path, errno)),
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
