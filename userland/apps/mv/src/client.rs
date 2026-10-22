//! The move engine: resolve each source's destination, rename it, and — when
//! the rename would cross a filesystem boundary — copy the source to the
//! destination and then remove the source.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::{Errno, RenameFlags};
use tairix_fscopy::{
    copy_attributes, AttrError, Attributes, DataCopy, DataError, Omissions, PathFile, Sparse,
    CHUNK_LEN,
};
use tairix_help::{own_short_help, HelpSource};
use tairix_path::{join, leaf_name};

use crate::command::{Clobber, Command, Options, TargetMode};
use crate::error::MvError;
use crate::io::{Entry, EntryKind, FileSystem, Output, Prompt, RenameOutcome};

/// The usage banner a usage error is reported with, and the fallback the
/// short-help switches print when `mv`'s own Help tree is unavailable.
pub const USAGE: &str = "\
usage: mv [-finvT] [-t dir] [--] source... dest

  -f, --force                remove a blocking destination and retry the
                             rename; never prompt
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

A move to another volume copies each file with its holes and extended
attributes, and each link as a link, then removes the source; attributes the
destination cannot keep are reported in one warning at the end.
";

/// `mv`'s own command word: the short-help switches render its own Help
/// document through the same engine as any other command's.
const OWN_WORD: &str = "mv";

/// Run one [`Command`], moving its sources through `fs`. `locale` is the
/// user's `LANG` preference, if set; `help` is the tool's own `Help/` tree,
/// read by the short-help switches.
///
/// Each source is renamed onto its destination; a rename that would cross a
/// filesystem boundary falls back to a copy followed by removal of the source.
/// When the destination is an existing directory — and always with more than
/// one source or `-t` — each source is moved into it under its base name;
/// `-T` uses the destination exactly as given. An existing destination is
/// overwritten by default, skipped under `-n`, and asked about through
/// `prompt` under `-i` (a declined question skips that move without error).
/// `-v` reports each move on `out`; otherwise `mv` writes nothing on success
/// beyond the [`Command::Help`] banner.
///
/// The first failure stops the run before any later operand (fail closed).
///
/// # Errors
///
/// * [`MvError::Usage`] — more than one source aimed at a non-directory
///   destination (or at any destination under `-T`), or a `-t` operand
///   that is not an existing directory.
/// * [`MvError::Stat`] — an operand could not be inspected; carries the
///   underlying [`Errno`].
/// * [`MvError::Rename`] — a rename failed for a non-boundary reason.
/// * [`MvError::Read`] / [`MvError::Create`] / [`MvError::Write`] — a
///   cross-device copy failed.
/// * [`MvError::Remove`] — the source could not be removed after a copy.
/// * [`MvError::Prompt`] — a confirmation could not be read (never treated
///   as consent).
/// * [`MvError::Output`] — writing the usage banner or a `-v` report
///   failed.
/// * [`MvError::Attribute`] — the destination failed an extended attribute
///   outright.
///
/// Attributes a destination could not keep are reported once, after the
/// last move and before any failure that ended the run: a warning on
/// `diagnostics` and an advisory record on `out`'s information stream.
pub fn run(
    command: Command,
    locale: Option<&str>,
    fs: &dyn FileSystem,
    prompt: &dyn Prompt,
    help: &dyn HelpSource,
    out: &dyn Output,
    diagnostics: &dyn Output,
) -> Result<(), MvError> {
    match command {
        Command::Help => short_help(locale, help, out),
        Command::Move {
            options,
            sources,
            dest,
        } => {
            let mut mover = Mover {
                options,
                fs,
                prompt,
                out,
                omissions: Omissions::default(),
                buf: Vec::new(),
            };
            let outcome = mover.move_all(&sources, &dest);
            if let Some(report) = mover.omissions.report(OWN_WORD) {
                out.info(&report.record);
                // The warning is advisory: a stream refusing it changes nothing.
                let _ = diagnostics
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

/// One `mv` run: its options, its seams, and what its copies across a volume
/// boundary could not carry.
struct Mover<'a> {
    options: Options,
    fs: &'a dyn FileSystem,
    prompt: &'a dyn Prompt,
    out: &'a dyn Output,
    /// What the fallback's destinations could not keep, reported once.
    omissions: Omissions,
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
                None => return Err(MvError::Stat(Errno::NotFound)),
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
            if self.options.verbose {
                self.out
                    .write_all(format!("renamed '{source}' -> '{target}'\n").as_bytes())
                    .map_err(MvError::Output)?;
            }
        }
        Ok(())
    }

    /// Move one named source operand, a link as itself, to `target`.
    fn move_operand(&mut self, source: &str, target: &str) -> Result<(), MvError> {
        let kind = self.fs.kind(source).map_err(MvError::Stat)?;
        let occupied = own(target, self.fs)?.is_some();
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
        let outcome = match self.fs.rename(source, target, flags) {
            Err(Errno::AlreadyExists) if flags.refuses_replace() => {
                if !self.may_replace(target)? {
                    return Ok(());
                }
                self.fs.rename(source, target, RenameFlags::empty())
            }
            other => other,
        };
        match outcome {
            Ok(RenameOutcome::Renamed) => Ok(()),
            Ok(RenameOutcome::CrossDevice) => self.relocate(source, kind, target),
            Err(_) if self.options.force => self.force_retry(source, kind, target),
            Err(errno) => Err(MvError::Rename(errno)),
        }
    }

    /// Whether the existing `target` may be replaced: always by default,
    /// never under `-n`, and on the user's answer under `-i`.
    fn may_replace(&self, target: &str) -> Result<bool, MvError> {
        match self.options.clobber {
            Clobber::Overwrite => Ok(true),
            Clobber::Skip => Ok(false),
            Clobber::Prompt => self
                .prompt
                .confirm(&format!("overwrite '{target}'?"))
                .map_err(MvError::Prompt),
        }
    }

    /// `-f` recovery: remove a destination that blocks the rename, then
    /// retry the rename exactly once, still honouring a cross-device result.
    fn force_retry(&mut self, source: &str, kind: EntryKind, target: &str) -> Result<(), MvError> {
        // The destination blocked the rename (e.g. it exists and is not
        // writable). Remove it and retry once; a removal error is irrelevant
        // if the retried rename then succeeds.
        let _ = self.fs.remove_file(target);
        match self.fs.rename(source, target, RenameFlags::empty()) {
            Ok(RenameOutcome::Renamed) => Ok(()),
            Ok(RenameOutcome::CrossDevice) => self.relocate(source, kind, target),
            Err(errno) => Err(MvError::Rename(errno)),
        }
    }

    /// Relocate `source` to `target` across a filesystem boundary: copy the
    /// whole object (recursively, for a directory) and then remove the
    /// source.
    fn relocate(&mut self, source: &str, kind: EntryKind, target: &str) -> Result<(), MvError> {
        self.copy_tree(source, kind, target)?;
        remove_tree(source, kind, self.fs)
    }

    /// Copy an object whose [`EntryKind`] is known to `target`, recursing
    /// into directories.
    fn copy_tree(&mut self, source: &str, kind: EntryKind, target: &str) -> Result<(), MvError> {
        match kind {
            EntryKind::File => self.copy_file(source, target),
            EntryKind::Symlink => {
                let stored = self.fs.read_link(source).map_err(MvError::Read)?;
                // A link is created, never written over: the name it
                // replaces goes first, as the rename would have taken it.
                if own(target, self.fs)?.is_some() {
                    self.fs.remove_file(target).map_err(MvError::Create)?;
                }
                self.fs.symlink(&stored, target).map_err(MvError::Create)
            }
            EntryKind::Directory => {
                match resolved(target, self.fs)? {
                    Some(EntryKind::Directory) => {}
                    _ => self.fs.mkdir(target).map_err(MvError::Create)?,
                }
                self.carry_attributes(source, target)?;
                for entry in read_children(source, self.fs)? {
                    let child_source = join(source, &entry.name);
                    let child_target = join(target, &entry.name);
                    self.copy_tree(&child_source, entry.kind, &child_target)?;
                }
                Ok(())
            }
        }
    }

    /// Copy a regular file from `source` to `target`: its bytes with its
    /// holes, then its extended attributes. A copy that fails is removed:
    /// the move did not happen, and its source is kept.
    fn copy_file(&mut self, source: &str, target: &str) -> Result<(), MvError> {
        let len = self.fs.size(source).map_err(MvError::Read)?;
        if self.buf.is_empty() && !tairix_util::fallible::grow_to(&mut self.buf, CHUNK_LEN, 0) {
            return Err(MvError::Write(Errno::OutOfMemory));
        }
        self.fs.create(target).map_err(MvError::Create)?;
        let copied = DataCopy::new(len, Sparse::Auto)
            .run(
                &mut PathFile::new(self.fs, source),
                &mut PathFile::new(self.fs, target),
                &mut self.buf,
            )
            .map_err(|failure| match failure {
                DataError::Read(errno) => MvError::Read(errno),
                DataError::Write(errno) => MvError::Write(errno),
            })
            .and_then(|()| self.carry_attributes(source, target));
        if copied.is_err() {
            // Best-effort: the failure that ended the copy is the one told.
            let _ = self.fs.remove_file(target);
        }
        copied
    }

    /// Carry `source`'s extended attributes to `target`, noting what its
    /// volume could not keep: the source is removed next, so nothing it
    /// carried is left unsaid.
    fn carry_attributes(&mut self, source: &str, target: &str) -> Result<(), MvError> {
        let left =
            copy_attributes(self.fs, source, target, Attributes::Carry).map_err(
                |err| match err {
                    AttrError::Read(errno) => MvError::Read(errno),
                    AttrError::Write { key, error } => MvError::Attribute {
                        key,
                        target: String::from(target),
                        error,
                    },
                },
            )?;
        self.omissions.note(&left);
        Ok(())
    }
}

/// Remove an already-copied object whose [`EntryKind`] is known,
/// depth-first for a directory so a parent is removed only after its
/// contents; a link is unlinked itself, never followed.
fn remove_tree(source: &str, kind: EntryKind, fs: &dyn FileSystem) -> Result<(), MvError> {
    match kind {
        EntryKind::File | EntryKind::Symlink => fs.remove_file(source).map_err(MvError::Remove),
        EntryKind::Directory => {
            for entry in read_children(source, fs)? {
                let child = join(source, &entry.name);
                remove_tree(&child, entry.kind, fs)?;
            }
            fs.remove_dir(source).map_err(MvError::Remove)
        }
    }
}

/// Read every entry of `path` into a vector, so the directory can be walked
/// without depending on entry indices staying stable as the move proceeds.
fn read_children(path: &str, fs: &dyn FileSystem) -> Result<Vec<Entry>, MvError> {
    let mut entries = Vec::new();
    let mut index: u64 = 0;
    while let Some(entry) = fs.read_dir(path, index).map_err(MvError::Read)? {
        index = index.saturating_add(1);
        entries.push(entry);
    }
    Ok(entries)
}

/// What `path` names itself, a missing path [`None`], so a destination can be
/// probed for existence without treating absence as a failure.
fn own(path: &str, fs: &dyn FileSystem) -> Result<Option<EntryKind>, MvError> {
    absent_as_none(fs.kind(path))
}

/// What `path` resolves to, a final link followed; a missing path [`None`].
fn resolved(path: &str, fs: &dyn FileSystem) -> Result<Option<EntryKind>, MvError> {
    absent_as_none(fs.resolved_kind(path))
}

fn absent_as_none(probe: Result<EntryKind, Errno>) -> Result<Option<EntryKind>, MvError> {
    match probe {
        Ok(kind) => Ok(Some(kind)),
        Err(Errno::NotFound) => Ok(None),
        Err(errno) => Err(MvError::Stat(errno)),
    }
}
#[cfg(test)]
mod tests;
