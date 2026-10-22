//! The copy engine: resolve each source's destination, copy regular files
//! through the shared engine with their holes and extended attributes, and
//! reproduce each directory `-r` must copy — creating the destination
//! directory before its contents.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::{Errno, FileId};
use tairix_collections::HashMap;
use tairix_fscopy::{
    copy_attributes, landing, AttrError, Created, DataCopy, DataError, Descent, Landing, Omissions,
    PathFile, CHUNK_LEN,
};
use tairix_hash::BuildSipHash13;
use tairix_help::{own_short_help, HelpSource};
use tairix_path::{join, leaf_name};
use tairix_util::quote::quote;

use crate::command::{Clobber, Command, Contents, Dereference, Options, TargetMode};
use crate::error::CpError;
use crate::io::{Entry, EntryKind, FileSystem, Follow, Output, Probe, Prompt};

/// The usage banner a usage error is reported with, and the fallback the
/// short-help switches print when `cp`'s own Help tree is unavailable.
pub const USAGE: &str = "\
usage: cp [-dfHilLnPrRsvT] [--sparse=WHEN] [--preserve=LIST]
          [--no-preserve=LIST] [-t dir] [--] source... dest

  -r, -R, --recursive        copy directories and their contents
  -f, --force                remove an unwritable destination and retry
  -i, --interactive          ask before overwriting an existing file
  -n, --no-clobber           never overwrite an existing file
  -l, --link                 give the destination a second name for the
                             source's node instead of copying its bytes
  -s, --symbolic-link        make a symbolic link naming the source
  -P, --no-dereference       reproduce every symbolic-link source as a link
                             rather than copying what it names
  -L, --dereference          follow every symbolic-link source
  -H                         follow the operands' links, not those inside
                             a tree; without -H, -L or -P, a recursive copy
                             follows none and any other follows its operands
  --preserve=links           two sources naming one node get two names at
                             the destination, not two copies
  --preserve=xattr           fail a copy whose destination cannot keep an
                             extended attribute
  --no-preserve=xattr        copy no extended attributes
  --sparse=WHEN              auto (keep holes), always (also leave stored
                             zeros unwritten), never (write every byte)
  -d                         -P and --preserve=links together
  -v, --verbose              report each copy
  -t dir, --target-directory=dir
                             copy every source into dir
  -T, --no-target-directory  treat dest as a normal file (one source)
  -h, -?, --help             show this message

With one source and a non-directory dest, the source is copied to dest. When
dest is an existing directory (always, with more than one source) each source
is copied into it under its base name. `--` ends option parsing: every later
argument is a path.

Extended attributes (a RISC OS filetype, a Mac type and creator) are copied
wherever the destination keeps them; the ones it cannot keep are reported in
one warning at the end.

-a/--archive and the other --preserve members are refused: --preserve=all
includes timestamps, which no call can set yet, so they are not honoured in
part. Use -dR for the rest of -a.
";

/// `cp`'s own command word: the short-help switches render its own Help
/// document through the same engine as any other command's.
const OWN_WORD: &str = "cp";

/// Run one [`Command`], copying its sources through `fs`. `locale` is the
/// user's `LANG` preference, if set; `help` is the tool's own `Help/` tree,
/// read by the short-help switches.
///
/// A non-directory source is streamed to its destination; a directory source
/// is reproduced only with `-r`. When the destination is an existing
/// directory — and always with more than one source or `-t` — each source is
/// copied into it under its base name; `-T` uses the destination exactly as
/// given. An existing destination file is overwritten by default, skipped
/// under `-n`, and asked about through `prompt` under `-i` (a declined
/// question skips that copy without error). `-v` reports each copy on `out`;
/// otherwise `cp` writes nothing on success beyond the [`Command::Help`]
/// banner.
///
/// The first failure stops the run before any later operand (fail closed).
///
/// # Errors
///
/// * [`CpError::Usage`] — more than one source aimed at a non-directory
///   destination (or at any destination under `-T`).
/// * [`CpError::IsDirectory`] — a directory source was named without `-r`.
/// * [`CpError::NotADirectory`] — a directory source's destination already
///   exists as a non-directory, or the `-t` operand is not an existing
///   directory.
/// * [`CpError::Stat`] — a path could not be inspected.
/// * [`CpError::Read`] — a source file, directory or link could not be read.
/// * [`CpError::Create`] — a destination file, directory or link could not be
///   made.
/// * [`CpError::Write`] — writing a destination file failed.
/// * [`CpError::Prompt`] — a confirmation could not be read (never treated
///   as consent).
/// * [`CpError::Output`] — writing the usage banner or a `-v` report failed.
/// * [`CpError::Attribute`] — the destination refused an extended attribute
///   the copy had to carry.
/// * [`CpError::JustCreated`] — an earlier operand put a non-directory
///   source's destination there.
///
/// Attributes a destination could not keep are reported once, after the
/// last copy and before any failure that ended the run: a warning on the
/// diagnostics stream and an advisory record on the information stream.
pub fn run(command: Command, locale: Option<&str>, seams: Seams<'_>) -> Result<(), CpError> {
    match command {
        Command::Help => short_help(locale, seams.help, seams.out),
        Command::Copy {
            options,
            sources,
            dest,
        } => {
            let mut copier = Copier {
                options,
                linked: Linked::new(seams.keys),
                created: Created::new(seams.keys),
                omissions: Omissions::default(),
                descent: Descent::default(),
                buf: Vec::new(),
                fs: seams.fs,
                prompt: seams.prompt,
                out: seams.out,
            };
            let outcome = copier.copy_all(&sources, &dest);
            if let Some(report) = copier.omissions.report(OWN_WORD) {
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

/// What a run reaches outside itself through.
#[derive(Clone, Copy)]
pub struct Seams<'a> {
    /// The files copied.
    pub fs: &'a dyn FileSystem,
    /// The `-i` question.
    pub prompt: &'a dyn Prompt,
    /// The tool's own `Help/` tree, read by the short-help switches.
    pub help: &'a dyn HelpSource,
    /// The `-v` reports, the usage banner and the information stream.
    pub out: &'a dyn Output,
    /// The end-of-run warning about attributes left behind.
    pub diagnostics: &'a dyn Output,
    /// The process's hash key, which the run's records of nodes are kept
    /// under, since what names them may be a hostile volume's to choose.
    pub keys: BuildSipHash13,
}

/// Render `cp`'s own short help (`NAME` + `SYNOPSIS` + compact `OPTIONS`)
/// from its own Help tree through the one shared engine; when no document
/// can be served (a build without the bundle's documents) the usage banner
/// stands in — the tool's own text, not fabricated help content — so `-h`
/// never fails.
fn short_help(
    locale: Option<&str>,
    help: &dyn HelpSource,
    out: &dyn Output,
) -> Result<(), CpError> {
    let bytes =
        own_short_help(help, locale, OWN_WORD).unwrap_or_else(|| String::from(USAGE).into_bytes());
    out.write_all(&bytes).map_err(CpError::Output)
}

/// One `cp` run: its options, its seams, and the sharing it has preserved so
/// far.
///
/// Bundling them is what keeps every step a two- or three-argument method
/// rather than an eight-argument function — the shape `du`'s `Reporter` uses
/// for the same reason.
struct Copier<'a> {
    options: Options,
    /// The destinations already written for each multiply-named source node
    /// (`--preserve=links`).
    linked: Linked,
    /// The destinations earlier operands put in place.
    created: Created,
    /// What the copies' destinations could not keep, reported once.
    omissions: Omissions,
    /// The directories the walk is inside, so it never enters one again.
    descent: Descent,
    /// The one buffer every file's bytes pass through, sized on first use.
    buf: Vec<u8>,
    fs: &'a dyn FileSystem,
    prompt: &'a dyn Prompt,
    out: &'a dyn Output,
}

/// The destinations already written for each multiply-named source node, so
/// `--preserve=links` gives a second source naming one node a second *name*
/// rather than a second copy.
///
/// Only a node whose name count exceeds one is remembered — a node named once
/// cannot be met twice — so the map holds the hard links a copy actually
/// meets rather than one entry per node on the volume. It grows on demand
/// from no fixed ceiling; a heap that refuses to grow it makes the copy fall
/// back to copying the bytes, which is correct output rather than a failure,
/// and the destinations simply are not linked.
struct Linked {
    /// The destination first written for each source node.
    seen: HashMap<FileId, String, BuildSipHash13>,
}

impl Linked {
    const fn new(keys: BuildSipHash13) -> Self {
        Self {
            seen: HashMap::with_hasher(keys),
        }
    }

    /// The destination already written for `probe`'s node, if any.
    fn destination_of(&self, probe: &Probe) -> Option<&str> {
        if probe.nlink <= 1 || probe.id.is_none() {
            return None;
        }
        self.seen.get(&probe.id).map(String::as_str)
    }

    /// Remember `target` as the destination written for `probe`'s node.
    ///
    /// A node that cannot recur, or one the backing offers no identity for,
    /// is not remembered; nor is anything, if the heap cannot grow the map.
    fn remember(&mut self, probe: &Probe, target: &str) {
        if probe.nlink <= 1 || probe.id.is_none() || self.seen.contains_key(&probe.id) {
            return;
        }
        let mut name = String::new();
        if name.try_reserve_exact(target.len()).is_err() {
            return;
        }
        name.push_str(target);
        let _ = self.seen.try_insert(probe.id, name);
    }
}

impl Copier<'_> {
    /// Copy every source to `dest`, deciding per source whether the
    /// destination is `dest` itself or a child of `dest` named after the
    /// source.
    fn copy_all(&mut self, sources: &[String], dest: &str) -> Result<(), CpError> {
        // A destination is always probed through a final link: a directory
        // reached by a link is still the directory the copies land in, and
        // `-P`/`-d` are about how *sources* are read, never where output goes.
        let dest_is_dir = match self.options.target_mode {
            // `-t`: the destination must be an existing directory.
            TargetMode::Directory => match self.stat(dest, Follow::Target)?.map(|p| p.kind) {
                Some(EntryKind::Directory) => true,
                Some(EntryKind::File | EntryKind::Symlink) => return Err(CpError::NotADirectory),
                None => return Err(stat_error(dest, Errno::NotFound)),
            },
            // `-T`: the destination is a normal file for exactly one source.
            TargetMode::NoDirectory => {
                if sources.len() > 1 {
                    return Err(CpError::Usage);
                }
                false
            }
            TargetMode::Inferred => matches!(
                self.stat(dest, Follow::Target)?.map(|p| p.kind),
                Some(EntryKind::Directory)
            ),
        };
        // More than one source can only land inside a directory.
        if sources.len() > 1 && !dest_is_dir {
            return Err(CpError::Usage);
        }
        for source in sources {
            let target = if dest_is_dir {
                join(dest, leaf_name(source))
            } else {
                String::from(dest)
            };
            let probe = self
                .fs
                .probe(source, self.operand_follow())
                .map_err(|errno| stat_error(source, errno))?;
            if probe.kind == EntryKind::Directory && self.options.recursive {
                self.refuse_landing_in(source, probe.id, &target)?;
            }
            // A directory merges into one an earlier operand made, as GNU's
            // does; anything else would replace it.
            let guarded = probe.kind != EntryKind::Directory;
            let follow = self.destination_follow(&probe);
            if guarded {
                if let Some(there) = self.stat(&target, follow)? {
                    if self.created.holds(there.id, &target) {
                        return Err(CpError::JustCreated {
                            source: String::from(source),
                            target,
                        });
                    }
                }
            }
            let written = self.copy_known(source, probe, &target)?;
            if guarded && written {
                if let Some(made) = self.stat(&target, follow)? {
                    self.created
                        .note(made.id, &target)
                        .map_err(|errno| write_error(&target, errno))?;
                }
            }
        }
        Ok(())
    }

    /// How a destination for `probe` is looked at: through a final link for a
    /// byte copy, which writes what the link names, and as itself for a link
    /// or a second name, which replace the name.
    fn destination_follow(&self, probe: &Probe) -> Follow {
        if probe.kind == EntryKind::File && self.options.contents == Contents::Bytes {
            Follow::Target
        } else {
            Follow::Keep
        }
    }

    /// Refuse, before anything is written, a directory copy whose `target`
    /// is the directory `source` or lies inside it.
    fn refuse_landing_in(&self, source: &str, id: FileId, target: &str) -> Result<(), CpError> {
        let named = || (String::from(source), String::from(target));
        match landing(self.fs, source, id, target).map_err(|errno| stat_error(target, errno))? {
            Landing::Apart => Ok(()),
            Landing::Same => {
                let (source, target) = named();
                Err(CpError::SameFile { source, target })
            }
            Landing::Within => {
                let (source, target) = named();
                Err(CpError::IntoItself { source, target })
            }
        }
    }

    /// Copy an object a probe already described (from the parent's directory
    /// entry, or from the operand's own probe), recursing into directories;
    /// whether the destination was written rather than kept.
    fn copy_known(&mut self, source: &str, probe: Probe, target: &str) -> Result<bool, CpError> {
        match probe.kind {
            EntryKind::File => self.reproduce_file(source, &probe, target),
            // Only a `Follow::Keep` probe reports a link, i.e. only under
            // `-P`/`-d`: the link itself is reproduced by storing the same
            // target, verbatim, so a relative or dangling one survives.
            EntryKind::Symlink => {
                if !self.replaceable(target)? {
                    return Ok(false);
                }
                let stored = self
                    .fs
                    .read_link(source)
                    .map_err(|errno| read_error(source, errno))?;
                self.fs
                    .symlink(&stored, target)
                    .map_err(|errno| create_error(target, errno))?;
                self.report(source, target)?;
                Ok(true)
            }
            EntryKind::Directory => self
                .reproduce_directory(source, probe.id, target)
                .map(|()| true),
        }
    }

    /// Reproduce the directory source `source`, whose identity is `id`, under
    /// `target` and recurse into it.
    fn reproduce_directory(
        &mut self,
        source: &str,
        id: FileId,
        target: &str,
    ) -> Result<(), CpError> {
        if !self.options.recursive {
            return Err(CpError::IsDirectory);
        }
        let made = match self.stat(target, Follow::Target)? {
            Some(existing) if existing.kind == EntryKind::Directory => existing.id,
            Some(_) => return Err(CpError::NotADirectory),
            None => {
                self.fs
                    .mkdir(target)
                    .map_err(|errno| create_error(target, errno))?;
                self.carry_attributes(source, target)?;
                self.report(source, target)?;
                self.fs
                    .probe(target, Follow::Keep)
                    .map_err(|errno| stat_error(target, errno))?
                    .id
            }
        };
        self.descent
            .enter(id, made)
            .map_err(|errno| write_error(target, errno))?;
        for entry in self.read_children(source)? {
            let child_source = join(source, &entry.name);
            let child_target = join(target, &entry.name);
            // A listing describes each child itself, so a child link needs a
            // following probe when the run follows links inside a tree.
            let child = if entry.probe.kind == EntryKind::Symlink && self.follows_children() {
                self.fs
                    .probe(&child_source, Follow::Target)
                    .map_err(|errno| stat_error(&child_source, errno))?
            } else {
                entry.probe
            };
            if child.kind == EntryKind::Directory && self.descent.refuses(child.id) {
                return Err(CpError::IntoItself {
                    source: child_source,
                    target: child_target,
                });
            }
            self.copy_known(&child_source, child, &child_target)?;
        }
        self.descent.leave();
        Ok(())
    }

    /// Put a non-directory source at `target`: its bytes, a second name for
    /// its node (`-l`, or `--preserve=links` meeting the node again), or a
    /// symbolic link naming it (`-s`).
    fn reproduce_file(
        &mut self,
        source: &str,
        probe: &Probe,
        target: &str,
    ) -> Result<bool, CpError> {
        // A node already written under another of its names becomes a second
        // name there, whatever the contents mode: `--preserve=links` is about
        // keeping the *sources'* sharing, so it outranks copying the bytes.
        if let Some(first) = self.linked.destination_of(probe) {
            // The borrow of `self.linked` ends with this copy of the name.
            let first = String::from(first);
            if !self.replaceable(target)? {
                return Ok(false);
            }
            self.fs
                .link(&first, target)
                .map_err(|errno| create_error(target, errno))?;
            self.report(source, target)?;
            return Ok(true);
        }
        let written = match self.options.contents {
            Contents::Bytes => self.copy_file(source, probe, target)?,
            Contents::HardLink => {
                if !self.replaceable(target)? {
                    return Ok(false);
                }
                // Linking never follows a name, so a link this copy followed
                // is linked by the path of what it names, as GNU links it.
                let existing = match self.stat(source, Follow::Keep)? {
                    Some(own) if own.kind == EntryKind::Symlink => self
                        .fs
                        .canonical(source)
                        .map_err(|errno| stat_error(source, errno))?,
                    _ => String::from(source),
                };
                self.fs
                    .link(&existing, target)
                    .map_err(|errno| create_error(target, errno))?;
                self.report(source, target)?;
                true
            }
            Contents::SymbolicLink => {
                if !self.replaceable(target)? {
                    return Ok(false);
                }
                self.fs
                    .symlink(source, target)
                    .map_err(|errno| create_error(target, errno))?;
                self.report(source, target)?;
                true
            }
        };
        // A destination kept rather than written is no copy of this node.
        if written && self.options.preserve_links {
            self.linked.remember(probe, target);
        }
        Ok(written)
    }

    /// Whether the destination may be written: the existing-destination
    /// policy (`-n` skips, `-i` asks), plus the removal `-f` needs before a
    /// *create* that cannot overwrite.
    ///
    /// A link or a second name is created, and a create never replaces a
    /// name, so an occupied destination must be removed first — unlike the
    /// byte copy, which truncates through its own create. Without `-f` the
    /// create fails and the kernel says why.
    fn replaceable(&self, target: &str) -> Result<bool, CpError> {
        if self.stat(target, Follow::Keep)?.is_none() {
            return Ok(true);
        }
        if !self.consented(target)? {
            return Ok(false);
        }
        if self.options.force {
            // A removal error is irrelevant if the create then succeeds, and
            // is reported by the create if it does not.
            let _ = self.fs.remove_file(target);
        }
        Ok(true)
    }

    /// The existing-destination policy for a destination that is already
    /// there: `-n` skips it, `-i` asks, and the default overwrites.
    fn consented(&self, target: &str) -> Result<bool, CpError> {
        match self.options.clobber {
            Clobber::Overwrite => Ok(true),
            Clobber::Skip => Ok(false),
            Clobber::Prompt => self
                .prompt
                .confirm(&format!("overwrite {}?", quote(target)))
                .map_err(CpError::Prompt),
        }
    }

    /// Copy the regular file `source`, the node `node` describes, to `target`
    /// — its bytes with its holes, then its extended attributes — honouring
    /// the existing-destination policy and `-f` (remove a destination that
    /// cannot be created and retry the create once); whether it was written.
    fn copy_file(&mut self, source: &str, node: &Probe, target: &str) -> Result<bool, CpError> {
        let occupant = self.stat(target, Follow::Keep)?;
        // The create follows a final link, so the node it would empty is the
        // one the link names.
        let emptied = match occupant {
            Some(link) if link.kind == EntryKind::Symlink => self.stat(target, Follow::Target)?,
            other => other,
        };
        if emptied.is_some_and(|emptied| !node.id.is_none() && emptied.id == node.id) {
            return Err(CpError::SameFile {
                source: String::from(source),
                target: String::from(target),
            });
        }
        if occupant.is_some() && !self.consented(target)? {
            return Ok(false);
        }
        let len = self
            .fs
            .size(source)
            .map_err(|errno| read_error(source, errno))?;
        self.create_destination(target)?;
        if self.buf.is_empty() && !tairix_util::fallible::grow_to(&mut self.buf, CHUNK_LEN, 0) {
            return Err(write_error(target, Errno::OutOfMemory));
        }
        let mut from = PathFile::new(self.fs, source);
        let mut to = PathFile::new(self.fs, target);
        DataCopy::new(len, self.options.sparse)
            .run(&mut from, &mut to, &mut self.buf)
            .map_err(|failure| match failure {
                DataError::Read(errno) => read_error(source, errno),
                DataError::Write(errno) => write_error(target, errno),
            })?;
        self.carry_attributes(source, target)?;
        self.report(source, target)?;
        Ok(true)
    }

    /// Carry `source`'s extended attributes to `target`, noting what its
    /// volume could not keep.
    fn carry_attributes(&mut self, source: &str, target: &str) -> Result<(), CpError> {
        let left =
            copy_attributes(self.fs, source, target, self.options.attributes).map_err(|err| {
                match err {
                    AttrError::Read(errno) => read_error(source, errno),
                    AttrError::Write { key, error } => CpError::Attribute {
                        key,
                        target: String::from(target),
                        error,
                    },
                }
            })?;
        self.omissions.note(&left);
        Ok(())
    }

    /// Create `target`, or — with `-f` — remove it and retry the create once.
    fn create_destination(&self, target: &str) -> Result<(), CpError> {
        match self.fs.create(target) {
            Ok(()) => Ok(()),
            Err(_) if self.options.force => {
                // The destination could not be created (e.g. it exists and is
                // not writable). `-f` removes it and retries exactly once; a
                // removal error is irrelevant if the retried create succeeds.
                let _ = self.fs.remove_file(target);
                self.fs
                    .create(target)
                    .map_err(|errno| create_error(target, errno))
            }
            Err(errno) => Err(create_error(target, errno)),
        }
    }

    /// Read every entry of `path` into a vector, so the directory can be
    /// walked without depending on entry indices staying stable as the copy
    /// proceeds.
    fn read_children(&self, path: &str) -> Result<Vec<Entry>, CpError> {
        let mut entries = Vec::new();
        let mut index: u64 = 0;
        while let Some(entry) = self
            .fs
            .read_dir(path, index)
            .map_err(|errno| read_error(path, errno))?
        {
            index = index.saturating_add(1);
            entries.push(entry);
        }
        Ok(entries)
    }

    /// Inspect `path`, mapping a missing path to [`None`] so a destination
    /// can be probed for existence without treating absence as a failure.
    fn stat(&self, path: &str, follow: Follow) -> Result<Option<Probe>, CpError> {
        match self.fs.probe(path, follow) {
            Ok(probe) => Ok(Some(probe)),
            Err(Errno::NotFound) => Ok(None),
            Err(errno) => Err(stat_error(path, errno)),
        }
    }

    /// The follow posture an operand's own probe uses: GNU's, where a
    /// recursive copy reproduces an operand's link unless `-H`/`-L` (or `-l`,
    /// which links what the link names) says otherwise.
    fn operand_follow(&self) -> Follow {
        let follows = match self.options.dereference {
            Dereference::Never => false,
            Dereference::Operands | Dereference::Always => true,
            Dereference::Default => {
                !self.options.recursive || self.options.contents == Contents::HardLink
            }
        };
        if follows {
            Follow::Target
        } else {
            Follow::Keep
        }
    }

    /// Whether a link met inside a tree is followed rather than reproduced.
    fn follows_children(&self) -> bool {
        match self.options.dereference {
            Dereference::Always => true,
            Dereference::Never | Dereference::Operands => false,
            Dereference::Default => self.options.contents == Contents::HardLink,
        }
    }

    /// Report one copy under `-v`, in the GNU wording.
    fn report(&self, source: &str, target: &str) -> Result<(), CpError> {
        if !self.options.verbose {
            return Ok(());
        }
        self.out
            .write_all(format!("{} -> {}\n", quote(source), quote(target)).as_bytes())
            .map_err(CpError::Output)
    }
}

fn stat_error(path: &str, errno: Errno) -> CpError {
    CpError::Stat {
        path: String::from(path),
        errno,
    }
}

fn read_error(path: &str, errno: Errno) -> CpError {
    CpError::Read {
        path: String::from(path),
        errno,
    }
}

fn create_error(path: &str, errno: Errno) -> CpError {
    CpError::Create {
        path: String::from(path),
        errno,
    }
}

fn write_error(path: &str, errno: Errno) -> CpError {
    CpError::Write {
        path: String::from(path),
        errno,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        create_error, read_error, run as engine_run, stat_error, write_error, Seams, USAGE,
    };
    use crate::command::{Clobber, Command, Contents, Dereference, Options, TargetMode};
    use crate::error::CpError;
    use crate::io::{
        Ancestry, AttributeStore, Entry, EntryKind, FileSystem, Follow, Output, PathFiles, Probe,
        Prompt,
    };
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;
    use core::cell::RefCell;
    use tairix_abi::fs::SeekRegion;
    use tairix_abi::{Errno, FileId};
    use tairix_hash::BuildSipHash13;
    use tairix_help::{HelpSource, SourceError};

    /// A prompt no non-interactive run may ever reach.
    struct NeverAsked;

    impl Prompt for NeverAsked {
        fn confirm(&self, question: &str) -> Result<bool, Errno> {
            panic!("unexpected prompt: {question}");
        }
    }

    /// A scripted prompt: answers in order, recording every question; an
    /// exhausted script fails the read.
    struct Answers {
        replies: RefCell<Vec<bool>>,
        asked: RefCell<Vec<String>>,
    }

    impl Answers {
        fn new(replies: &[bool]) -> Self {
            Self {
                replies: RefCell::new(replies.to_vec()),
                asked: RefCell::new(Vec::new()),
            }
        }

        fn asked(&self) -> Vec<String> {
            self.asked.borrow().clone()
        }
    }

    impl Prompt for Answers {
        fn confirm(&self, question: &str) -> Result<bool, Errno> {
            self.asked.borrow_mut().push(question.to_string());
            let mut replies = self.replies.borrow_mut();
            if replies.is_empty() {
                return Err(Errno::NotFound);
            }
            Ok(replies.remove(0))
        }
    }

    /// A Help tree with no documents at all: the short-help fallback path.
    struct NoHelp;

    impl HelpSource for NoHelp {
        fn locale_dirs(&self) -> Result<Vec<String>, SourceError> {
            Ok(Vec::new())
        }

        fn read(
            &self,
            _locale_dir: &str,
            _file_name: &str,
        ) -> Result<Option<Vec<u8>>, SourceError> {
            Ok(None)
        }
    }

    /// A Help tree holding one canonical `cp.md` document.
    struct OneDoc;

    const DOC: &str = "## NAME\n\ncp — copy files and directories\n\n\
                       ## SYNOPSIS\n\n`cp [-r] [--] source... dest`\n\n\
                       ## DESCRIPTION\n\nCopies things.\n";

    impl HelpSource for OneDoc {
        fn locale_dirs(&self) -> Result<Vec<String>, SourceError> {
            Ok(alloc::vec![String::from("en-US")])
        }

        fn read(&self, locale_dir: &str, file_name: &str) -> Result<Option<Vec<u8>>, SourceError> {
            if locale_dir == "en-US" && file_name == "cp.md" {
                Ok(Some(DOC.as_bytes().to_vec()))
            } else {
                Ok(None)
            }
        }
    }

    /// The engine under a prompt that must never be reached — the shape
    /// every pre-existing non-interactive test uses.
    fn run(command: Command, fs: &dyn FileSystem, out: &Recorder) -> Result<(), CpError> {
        engine_run(command, None, seams(fs, &NeverAsked, &NoHelp, out, out))
    }

    fn seams<'a>(
        fs: &'a dyn FileSystem,
        prompt: &'a dyn Prompt,
        help: &'a dyn HelpSource,
        out: &'a dyn Output,
        diagnostics: &'a dyn Output,
    ) -> Seams<'a> {
        Seams {
            fs,
            prompt,
            help,
            out,
            diagnostics,
            keys: BuildSipHash13::UNKEYED,
        }
    }

    /// An in-memory tree. Regular files carry their bytes; directories are
    /// named so their children can be derived by parent path. Failure
    /// injection covers the create/read/write fail-closed paths and the `-f`
    /// remove-then-recreate recovery.
    struct MemFs {
        state: RefCell<State>,
    }

    struct State {
        files: Vec<(String, Vec<u8>)>,
        dirs: Vec<String>,
        /// `create` of this path fails with the errno until it is removed.
        create_fail: Option<(String, Errno)>,
        /// `read` of this path fails with the errno.
        read_fail: Option<(String, Errno)>,
        /// `write` to this path fails with the errno.
        write_fail: Option<(String, Errno)>,
        /// Removals recorded in call order.
        removed: Vec<String>,
        /// Symbolic links: `(path, stored target)`, the target verbatim.
        symlinks: Vec<(String, String)>,
        /// Second names: `(path, the path whose node it also names)`, so a
        /// hard-linked pair shares one identity and one set of contents.
        hard: Vec<(String, String)>,
        /// The stored runs `(start, end)` of a sparse file; a file not listed
        /// is stored throughout.
        runs: Vec<(String, Vec<(u64, u64)>)>,
        /// Every write's `(path, offset, length)`, in order.
        writes: Vec<(String, u64, usize)>,
        /// Extended attributes: `(path, key, value)`.
        attrs: Vec<(String, String, Vec<u8>)>,
        /// Setting this key anywhere is refused with the errno.
        attr_refusal: Option<(String, Errno)>,
    }

    impl MemFs {
        fn new() -> Self {
            Self {
                state: RefCell::new(State {
                    files: Vec::new(),
                    dirs: Vec::new(),
                    create_fail: None,
                    read_fail: None,
                    write_fail: None,
                    removed: Vec::new(),
                    symlinks: Vec::new(),
                    hard: Vec::new(),
                    runs: Vec::new(),
                    writes: Vec::new(),
                    attrs: Vec::new(),
                    attr_refusal: None,
                }),
            }
        }

        /// A sparse file at `path`, `len` bytes long, storing `runs` of
        /// `(offset, bytes)`; its holes read as zeros.
        fn sparse(self, path: &str, len: usize, runs: &[(u64, &[u8])]) -> Self {
            let mut contents = alloc::vec![0u8; len];
            let mut stored = Vec::new();
            for (at, bytes) in runs {
                let at_usize = usize::try_from(*at).expect("fixture offset");
                contents[at_usize..at_usize + bytes.len()].copy_from_slice(bytes);
                stored.push((*at, *at + bytes.len() as u64));
            }
            self.state
                .borrow_mut()
                .runs
                .push((path.to_string(), stored));
            self.file(path, &contents)
        }

        /// An extended attribute on `path`.
        fn attr(self, path: &str, key: &str, value: &[u8]) -> Self {
            self.state
                .borrow_mut()
                .attrs
                .push((path.to_string(), key.to_string(), value.to_vec()));
            self
        }

        /// Setting `key` anywhere is refused with `errno`.
        fn refuses_attr(self, key: &str, errno: Errno) -> Self {
            self.state.borrow_mut().attr_refusal = Some((key.to_string(), errno));
            self
        }

        fn attr_of(&self, path: &str, key: &str) -> Option<Vec<u8>> {
            self.state
                .borrow()
                .attrs
                .iter()
                .find(|(p, k, _)| p == path && k == key)
                .map(|(_, _, value)| value.clone())
        }

        /// The bytes written to `path`, in total.
        fn written_to(&self, path: &str) -> usize {
            self.state
                .borrow()
                .writes
                .iter()
                .filter(|(p, _, _)| p == path)
                .map(|(_, _, len)| len)
                .sum()
        }

        fn file(self, path: &str, contents: &[u8]) -> Self {
            self.state
                .borrow_mut()
                .files
                .push((path.to_string(), contents.to_vec()));
            self
        }

        fn dir(self, path: &str) -> Self {
            self.state.borrow_mut().dirs.push(path.to_string());
            self
        }

        /// A symbolic link at `path` storing `target` verbatim.
        fn symlink_at(self, path: &str, target: &str) -> Self {
            self.state
                .borrow_mut()
                .symlinks
                .push((path.to_string(), target.to_string()));
            self
        }

        /// A second name at `path` for the node `object` already names.
        fn second_name(self, path: &str, object: &str) -> Self {
            self.state
                .borrow_mut()
                .hard
                .push((path.to_string(), object.to_string()));
            self
        }

        fn target_of(&self, path: &str) -> Option<String> {
            self.state
                .borrow()
                .symlinks
                .iter()
                .find(|(p, _)| p == path)
                .map(|(_, target)| target.clone())
        }

        fn names_of(&self, object: &str) -> usize {
            1 + self
                .state
                .borrow()
                .hard
                .iter()
                .filter(|(_, o)| o == object)
                .count()
        }

        fn create_fails(self, path: &str, errno: Errno) -> Self {
            self.state.borrow_mut().create_fail = Some((path.to_string(), errno));
            self
        }

        fn read_fails(self, path: &str, errno: Errno) -> Self {
            self.state.borrow_mut().read_fail = Some((path.to_string(), errno));
            self
        }

        fn write_fails(self, path: &str, errno: Errno) -> Self {
            self.state.borrow_mut().write_fail = Some((path.to_string(), errno));
            self
        }

        fn contents(&self, path: &str) -> Option<Vec<u8>> {
            self.state
                .borrow()
                .files
                .iter()
                .find(|(p, _)| p == path)
                .map(|(_, bytes)| bytes.clone())
        }

        fn has_dir(&self, path: &str) -> bool {
            self.state.borrow().dirs.iter().any(|p| p == path)
        }

        fn removed(&self) -> Vec<String> {
            self.state.borrow().removed.clone()
        }
    }

    /// Canonicalise a path the way a real filesystem would: a trailing slash
    /// on anything but the root names the same object (`/d/` is `/d`).
    fn canon(path: &str) -> &str {
        let trimmed = path.trim_end_matches('/');
        if trimmed.is_empty() {
            "/"
        } else {
            trimmed
        }
    }

    /// The immediate parent of a path: `/a/b` maps to `/a`, and a top-level
    /// `/a` maps to the empty string (its children sit at the root level).
    fn parent_of(path: &str) -> &str {
        match path.rfind('/') {
            Some(0) => "/",
            Some(slash) => &path[..slash],
            None => "",
        }
    }

    /// The final component of a path.
    fn name_of(path: &str) -> &str {
        match path.rfind('/') {
            Some(slash) => &path[slash + 1..],
            None => path,
        }
    }

    impl MemFs {
        /// The path whose file object `path` names: itself, or the object a
        /// second name shares. A symbolic link is **not** followed — the
        /// posture `link` and a `Follow::Keep` probe need.
        fn object_of(&self, path: &str) -> Option<String> {
            let state = self.state.borrow();
            if state.files.iter().any(|(p, _)| p == path) {
                return Some(path.to_string());
            }
            state
                .hard
                .iter()
                .find(|(p, _)| p == path)
                .map(|(_, object)| object.clone())
        }

        /// As [`MemFs::object_of`], but resolving a final symbolic link, the
        /// way a read through a following descriptor reaches its target.
        /// Bounded, so a cycle in the fixture cannot spin.
        fn followed_object_of(&self, path: &str) -> Option<String> {
            let mut name = path.to_string();
            for _ in 0..8 {
                if let Some(object) = self.object_of(&name) {
                    return Some(object);
                }
                name = self.target_of(&name)?;
            }
            None
        }

        /// The identity of the directory `path`, distinct from every file's.
        fn dir_id(&self, path: &str) -> FileId {
            let index = self
                .state
                .borrow()
                .dirs
                .iter()
                .position(|p| p == path)
                .expect("the directory is in the fixture");
            FileId {
                volume: [3u8; 16],
                node: u64::try_from(index).expect("fixture index") + 1,
            }
        }

        /// The identity and name count of the object at `object`: a distinct
        /// non-zero node number per object, plus one name per second name.
        fn object_identity(&self, object: &str) -> (FileId, u32) {
            let index = self
                .state
                .borrow()
                .files
                .iter()
                .position(|(p, _)| p == object)
                .expect("the object is in the fixture");
            (
                FileId {
                    volume: [2u8; 16],
                    node: u64::try_from(index).expect("fixture index") + 1,
                },
                u32::try_from(self.names_of(object)).expect("fixture names"),
            )
        }
    }

    impl PathFiles for MemFs {
        fn read(&self, path: &str, offset: u64, buf: &mut [u8]) -> Result<usize, Errno> {
            let named = canon(path);
            let object = self.followed_object_of(named).ok_or(Errno::NotFound)?;
            let path = object.as_str();
            let state = self.state.borrow();
            if let Some((p, errno)) = &state.read_fail {
                if p == named {
                    return Err(*errno);
                }
            }
            let (_, bytes) = state
                .files
                .iter()
                .find(|(p, _)| p == path)
                .ok_or(Errno::NotFound)?;
            let start = usize::try_from(offset).map_err(|_| Errno::LengthOutOfRange)?;
            if start >= bytes.len() {
                return Ok(0);
            }
            let take = (bytes.len() - start).min(buf.len());
            buf[..take].copy_from_slice(&bytes[start..start + take]);
            Ok(take)
        }
        fn size(&self, path: &str) -> Result<u64, Errno> {
            let object = self
                .followed_object_of(canon(path))
                .ok_or(Errno::NotFound)?;
            let state = self.state.borrow();
            let (_, bytes) = state
                .files
                .iter()
                .find(|(p, _)| *p == object)
                .ok_or(Errno::NotFound)?;
            Ok(bytes.len() as u64)
        }
        fn next_region(
            &self,
            path: &str,
            offset: u64,
            region: SeekRegion,
        ) -> Result<Option<u64>, Errno> {
            let object = self
                .followed_object_of(canon(path))
                .ok_or(Errno::NotFound)?;
            let len = self.size(&object)?;
            let state = self.state.borrow();
            let Some((_, runs)) = state.runs.iter().find(|(p, _)| *p == object) else {
                return Ok(region.in_solid_file(offset, len));
            };
            if offset >= len {
                return Ok(None);
            }
            let inside = runs
                .iter()
                .find(|(start, end)| (*start..*end).contains(&offset));
            Ok(match (region, inside) {
                (SeekRegion::Data, Some(_)) | (SeekRegion::Hole, None) => Some(offset),
                (SeekRegion::Hole, Some((_, end))) => Some(*end),
                (SeekRegion::Data, None) => runs
                    .iter()
                    .map(|(start, _)| *start)
                    .find(|&start| start > offset),
            })
        }
        fn set_len(&self, path: &str, len: u64) -> Result<(), Errno> {
            let path = canon(path);
            let mut state = self.state.borrow_mut();
            let (_, file) = state
                .files
                .iter_mut()
                .find(|(p, _)| p == path)
                .ok_or(Errno::NotFound)?;
            file.resize(
                usize::try_from(len).map_err(|_| Errno::LengthOutOfRange)?,
                0,
            );
            Ok(())
        }
        fn write(&self, path: &str, offset: u64, bytes: &[u8]) -> Result<(), Errno> {
            let path = canon(path);
            let mut state = self.state.borrow_mut();
            if let Some((p, errno)) = &state.write_fail {
                if p == path {
                    return Err(*errno);
                }
            }
            let start = usize::try_from(offset).map_err(|_| Errno::LengthOutOfRange)?;
            let (_, file) = state
                .files
                .iter_mut()
                .find(|(p, _)| p == path)
                .ok_or(Errno::NotFound)?;
            let end = start + bytes.len();
            if file.len() < end {
                file.resize(end, 0);
            }
            file[start..end].copy_from_slice(bytes);
            state.writes.push((path.to_string(), offset, bytes.len()));
            Ok(())
        }
    }

    impl Ancestry for MemFs {
        fn canonical(&self, path: &str) -> Result<String, Errno> {
            tairix_fscopy::fixture::resolve(path, |here| self.target_of(here))
        }

        fn identity(&self, path: &str) -> Result<FileId, Errno> {
            let path = canon(path);
            if path == "/" {
                return Ok(FileId {
                    volume: [3u8; 16],
                    node: u64::MAX,
                });
            }
            if self.target_of(path).is_some() {
                return Ok(FileId::NONE);
            }
            if self.state.borrow().dirs.iter().any(|p| p == path) {
                return Ok(self.dir_id(path));
            }
            let object = self.object_of(path).ok_or(Errno::NotFound)?;
            Ok(self.object_identity(&object).0)
        }
    }

    impl FileSystem for MemFs {
        fn probe(&self, path: &str, follow: Follow) -> Result<Probe, Errno> {
            let path = canon(path);
            if let Some(target) = self.target_of(path) {
                return match follow {
                    // A link describes itself: no second name can exist for
                    // one in this fixture, so one name is the whole truth.
                    Follow::Keep => Ok(Probe {
                        kind: EntryKind::Symlink,
                        id: FileId::NONE,
                        nlink: 1,
                    }),
                    // Following resolves to what the link names; a dangling
                    // one is the kernel's `NotFound`.
                    Follow::Target => self.probe(&target, follow),
                };
            }
            if self.state.borrow().dirs.iter().any(|p| p == path) {
                return Ok(Probe {
                    kind: EntryKind::Directory,
                    id: self.dir_id(path),
                    nlink: 2,
                });
            }
            let object = self.object_of(path).ok_or(Errno::NotFound)?;
            let (id, nlink) = self.object_identity(&object);
            Ok(Probe {
                kind: EntryKind::File,
                id,
                nlink,
            })
        }

        fn read_link(&self, path: &str) -> Result<String, Errno> {
            self.target_of(canon(path)).ok_or(Errno::OutOfRange)
        }

        fn symlink(&self, target: &str, link: &str) -> Result<(), Errno> {
            let link = canon(link).to_string();
            let mut state = self.state.borrow_mut();
            if state.symlinks.iter().any(|(p, _)| *p == link)
                || state.files.iter().any(|(p, _)| *p == link)
                || state.dirs.contains(&link)
            {
                return Err(Errno::AlreadyExists);
            }
            state.symlinks.push((link, target.to_string()));
            Ok(())
        }

        fn link(&self, existing: &str, new: &str) -> Result<(), Errno> {
            let object = self.object_of(canon(existing)).ok_or(Errno::NotFound)?;
            let new = canon(new).to_string();
            let mut state = self.state.borrow_mut();
            if state.files.iter().any(|(p, _)| *p == new)
                || state.hard.iter().any(|(p, _)| *p == new)
                || state.symlinks.iter().any(|(p, _)| *p == new)
            {
                return Err(Errno::AlreadyExists);
            }
            state.hard.push((new, object));
            Ok(())
        }

        fn read_dir(&self, path: &str, index: u64) -> Result<Option<Entry>, Errno> {
            let path = canon(path);
            let state = self.state.borrow();
            if !state.dirs.iter().any(|p| p == path) {
                return Err(Errno::NotFound);
            }
            // Derive the directory's children from every file and directory
            // whose immediate parent is `path`, in a stable insertion order.
            let mut names = Vec::new();
            for (p, _) in &state.files {
                if parent_of(p) == path {
                    names.push((p.clone(), EntryKind::File));
                }
            }
            for (p, _) in &state.hard {
                if parent_of(p) == path {
                    names.push((p.clone(), EntryKind::File));
                }
            }
            for (p, _) in &state.symlinks {
                if parent_of(p) == path {
                    names.push((p.clone(), EntryKind::Symlink));
                }
            }
            for p in &state.dirs {
                if parent_of(p) == path {
                    names.push((p.clone(), EntryKind::Directory));
                }
            }
            drop(state);
            // A listing describes each child itself, carrying the identity
            // and name count the real `fs_readdir` record now reports.
            let mut entries = Vec::new();
            for (p, kind) in names {
                let (id, nlink) = match kind {
                    EntryKind::File => self
                        .object_of(&p)
                        .map_or((FileId::NONE, 1), |object| self.object_identity(&object)),
                    EntryKind::Directory => (self.dir_id(&p), 2),
                    EntryKind::Symlink => (FileId::NONE, 1),
                };
                entries.push(Entry {
                    name: name_of(&p).to_string(),
                    probe: Probe { kind, id, nlink },
                });
            }
            let idx = usize::try_from(index).map_err(|_| Errno::LengthOutOfRange)?;
            Ok(entries.into_iter().nth(idx))
        }

        fn mkdir(&self, path: &str) -> Result<(), Errno> {
            self.state.borrow_mut().dirs.push(canon(path).to_string());
            Ok(())
        }

        fn create(&self, path: &str) -> Result<(), Errno> {
            let path = canon(path);
            let mut state = self.state.borrow_mut();
            if let Some((p, errno)) = &state.create_fail {
                if p == path {
                    return Err(*errno);
                }
            }
            if let Some((_, bytes)) = state.files.iter_mut().find(|(p, _)| p == path) {
                bytes.clear();
            } else {
                state.files.push((path.to_string(), Vec::new()));
            }
            Ok(())
        }

        fn remove_file(&self, path: &str) -> Result<(), Errno> {
            let path = canon(path);
            let mut state = self.state.borrow_mut();
            state.removed.push(path.to_string());
            state.files.retain(|(p, _)| p != path);
            // A removed destination is no longer blocked from being created.
            if matches!(&state.create_fail, Some((p, _)) if p == path) {
                state.create_fail = None;
            }
            Ok(())
        }
    }

    impl AttributeStore for MemFs {
        fn attr_keys(&self, path: &str) -> Result<Vec<String>, Errno> {
            Ok(self
                .state
                .borrow()
                .attrs
                .iter()
                .filter(|(p, _, _)| p == canon(path))
                .map(|(_, key, _)| key.clone())
                .collect())
        }

        fn attr_value(&self, path: &str, key: &str) -> Result<Vec<u8>, Errno> {
            self.attr_of(canon(path), key).ok_or(Errno::NoData)
        }

        fn attr_set(&self, path: &str, key: &str, value: &[u8]) -> Result<(), Errno> {
            let mut state = self.state.borrow_mut();
            if let Some((refused, errno)) = &state.attr_refusal {
                if refused == key {
                    return Err(*errno);
                }
            }
            state
                .attrs
                .push((canon(path).to_string(), key.to_string(), value.to_vec()));
            Ok(())
        }
    }

    /// Captures every byte written and every advisory record; optionally
    /// fails on the first write.
    struct Recorder {
        bytes: RefCell<Vec<u8>>,
        records: RefCell<Vec<u8>>,
        fail: bool,
    }

    impl Recorder {
        fn new() -> Self {
            Self {
                bytes: RefCell::new(Vec::new()),
                records: RefCell::new(Vec::new()),
                fail: false,
            }
        }

        fn failing() -> Self {
            Self {
                fail: true,
                ..Self::new()
            }
        }

        fn records(&self) -> String {
            String::from_utf8(self.records.borrow().clone()).expect("utf8 records")
        }

        fn text(&self) -> String {
            String::from_utf8(self.bytes.borrow().clone()).expect("utf8 output")
        }
    }

    impl Output for Recorder {
        fn write_all(&self, bytes: &[u8]) -> Result<(), Errno> {
            if self.fail {
                return Err(Errno::NotFound);
            }
            self.bytes.borrow_mut().extend_from_slice(bytes);
            Ok(())
        }

        fn info(&self, record: &[u8]) {
            self.records.borrow_mut().extend_from_slice(record);
        }
    }

    /// Run `command` with its diagnostics apart from its output.
    fn run_apart(
        command: Command,
        fs: &dyn FileSystem,
    ) -> (Result<(), CpError>, Recorder, Recorder) {
        let (out, diagnostics) = (Recorder::new(), Recorder::new());
        let result = engine_run(
            command,
            None,
            seams(fs, &NeverAsked, &NoHelp, &out, &diagnostics),
        );
        (result, out, diagnostics)
    }

    #[test]
    fn a_sparse_file_is_copied_with_its_holes() {
        let fs = MemFs::new().sparse("/disk.img", 1 << 20, &[(0, b"boot"), (512 * 1024, b"data")]);
        let (result, _, _) = run_apart(copy(false, false, &["/disk.img"], "/copy.img"), &fs);
        assert_eq!(result, Ok(()));
        assert_eq!(fs.contents("/copy.img"), fs.contents("/disk.img"));
        assert_eq!(
            fs.written_to("/copy.img"),
            8,
            "only the stored bytes are written"
        );
        let dense = Command::Copy {
            options: Options {
                sparse: tairix_fscopy::Sparse::Never,
                ..Options::DEFAULT
            },
            sources: alloc::vec![String::from("/disk.img")],
            dest: String::from("/dense.img"),
        };
        let (result, _, _) = run_apart(dense, &fs);
        assert_eq!(result, Ok(()));
        assert_eq!(fs.contents("/dense.img"), fs.contents("/disk.img"));
        assert_eq!(
            fs.written_to("/dense.img"),
            1 << 20,
            "--sparse=never writes every byte"
        );
    }

    #[test]
    fn extended_attributes_travel_with_files_and_directories() {
        let fs = MemFs::new()
            .dir("/app")
            .attr("/app", "acorn.attr", b"RWD/r")
            .file("/app/run", b"*Run")
            .attr("/app/run", "acorn.filetype", b"feb");
        let (result, _, diagnostics) = run_apart(copy(true, false, &["/app"], "/copied"), &fs);
        assert_eq!(result, Ok(()));
        assert_eq!(
            fs.attr_of("/copied", "acorn.attr").as_deref(),
            Some(&b"RWD/r"[..])
        );
        assert_eq!(
            fs.attr_of("/copied/run", "acorn.filetype").as_deref(),
            Some(&b"feb"[..])
        );
        assert_eq!(diagnostics.text(), "", "nothing was left behind");
    }

    #[test]
    fn attributes_the_destination_cannot_keep_are_reported_once() {
        let fs = MemFs::new()
            .file("/a", b"one")
            .attr("/a", "acorn.filetype", b"fff")
            .file("/b", b"two")
            .attr("/b", "acorn.filetype", b"ffd")
            .dir("/fat")
            .refuses_attr("acorn.filetype", Errno::NotSupported);
        let (result, out, diagnostics) = run_apart(copy(false, false, &["/a", "/b"], "/fat"), &fs);
        assert_eq!(result, Ok(()), "a carrying copy still copies");
        assert_eq!(fs.contents("/fat/b").as_deref(), Some(&b"two"[..]));
        assert_eq!(
            diagnostics.text(),
            "cp: warning: extended attributes not kept on 2 files (the destination cannot store \
             them): acorn.filetype\n"
        );
        assert_eq!(out.text(), "", "the warning is no output");
        assert!(
            out.records()
                .contains("\"code\":\"fs.attributes_not_kept\""),
            "{}",
            out.records()
        );

        let required = Command::Copy {
            options: Options {
                attributes: tairix_fscopy::Attributes::Require,
                ..Options::DEFAULT
            },
            sources: alloc::vec![String::from("/a")],
            dest: String::from("/fat/a2"),
        };
        let (result, _, _) = run_apart(required, &fs);
        assert_eq!(
            result,
            Err(CpError::Attribute {
                key: String::from("acorn.filetype"),
                target: String::from("/fat/a2"),
                error: Errno::NotSupported,
            })
        );

        let skipped = Command::Copy {
            options: Options {
                attributes: tairix_fscopy::Attributes::Skip,
                ..Options::DEFAULT
            },
            sources: alloc::vec![String::from("/a")],
            dest: String::from("/plain"),
        };
        let (result, _, diagnostics) = run_apart(skipped, &fs);
        assert_eq!(result, Ok(()));
        assert_eq!(fs.attr_of("/plain", "acorn.filetype"), None);
        assert_eq!(diagnostics.text(), "");
    }

    #[test]
    fn a_destination_that_is_the_source_itself_is_refused_and_left_whole() {
        let fs = MemFs::new()
            .file("/a", b"keep me")
            .symlink_at("/alias", "/a");
        fs.link("/a", "/second").expect("a second name");
        for dest in ["/a", "/second", "/alias"] {
            let (result, _, _) = run_apart(copy(false, true, &["/a"], dest), &fs);
            assert_eq!(
                result,
                Err(CpError::SameFile {
                    source: String::from("/a"),
                    target: String::from(dest),
                }),
                "{dest}"
            );
            assert_eq!(
                fs.contents("/a").as_deref(),
                Some(&b"keep me"[..]),
                "{dest}"
            );
        }
        let refused = CpError::SameFile {
            source: String::from("a"),
            target: String::from("a"),
        };
        assert_eq!(refused.to_string(), "'a' and 'a' are the same file");
    }

    #[test]
    fn an_attribute_key_holding_control_bytes_is_reported_escaped() {
        let refused = CpError::Attribute {
            key: String::from("user.\u{1b}[2J"),
            target: String::from("/fat/a"),
            error: Errno::NotSupported,
        };
        assert_eq!(
            refused.to_string(),
            alloc::format!(
                "setting attribute 'user.\\x1b[2J' for '/fat/a': {}",
                Errno::NotSupported
            )
        );
    }

    fn copy_with(options: Options, sources: &[&str], dest: &str) -> Command {
        Command::Copy {
            options,
            sources: sources.iter().map(|p| (*p).to_string()).collect::<Vec<_>>(),
            dest: dest.to_string(),
        }
    }

    fn copy(recursive: bool, force: bool, sources: &[&str], dest: &str) -> Command {
        copy_with(
            Options {
                recursive,
                force,
                ..Options::DEFAULT
            },
            sources,
            dest,
        )
    }

    /// Assertions about the fixture state a link-making run produced.
    impl MemFs {
        fn is_second_name_of(&self, path: &str, object: &str) -> bool {
            self.state
                .borrow()
                .hard
                .iter()
                .any(|(p, o)| p == path && o == object)
        }
    }

    // --- -l: a second name instead of a copy ------------------------------

    #[test]
    fn link_gives_the_destination_a_second_name_rather_than_copying() {
        // The point of `-l`: no second copy of the bytes exists, so the
        // destination cannot diverge from the source on the next write.
        let fs = MemFs::new().file("/a.txt", b"hello");
        let out = Recorder::new();
        assert_eq!(
            run(
                copy_with(
                    Options {
                        contents: Contents::HardLink,
                        ..Options::DEFAULT
                    },
                    &["/a.txt"],
                    "/b.txt"
                ),
                &fs,
                &out
            ),
            Ok(())
        );
        assert!(fs.is_second_name_of("/b.txt", "/a.txt"));
        assert!(
            fs.contents("/b.txt").is_none(),
            "no second copy of the bytes"
        );
        assert_eq!(fs.names_of("/a.txt"), 2);
    }

    #[test]
    fn link_onto_a_taken_name_is_refused_without_force() {
        // A create never replaces a name, so the kernel's `AlreadyExists`
        // reaches the caller rather than a silent overwrite.
        let fs = MemFs::new().file("/a.txt", b"hello").file("/b.txt", b"old");
        let out = Recorder::new();
        assert_eq!(
            run(
                copy_with(
                    Options {
                        contents: Contents::HardLink,
                        ..Options::DEFAULT
                    },
                    &["/a.txt"],
                    "/b.txt"
                ),
                &fs,
                &out
            ),
            Err(create_error("/b.txt", Errno::AlreadyExists))
        );
        assert_eq!(fs.contents("/b.txt"), Some(b"old".to_vec()));
    }

    #[test]
    fn force_removes_the_taken_name_before_linking() {
        let fs = MemFs::new().file("/a.txt", b"hello").file("/b.txt", b"old");
        let out = Recorder::new();
        assert_eq!(
            run(
                copy_with(
                    Options {
                        contents: Contents::HardLink,
                        force: true,
                        ..Options::DEFAULT
                    },
                    &["/a.txt"],
                    "/b.txt"
                ),
                &fs,
                &out
            ),
            Ok(())
        );
        assert_eq!(fs.removed(), Vec::from(["/b.txt".to_string()]));
        assert!(fs.is_second_name_of("/b.txt", "/a.txt"));
    }

    // --- -s: a symbolic link naming the source ---------------------------

    #[test]
    fn symbolic_link_names_the_source_rather_than_copying_it() {
        let fs = MemFs::new().file("/a.txt", b"hello");
        let out = Recorder::new();
        assert_eq!(
            run(
                copy_with(
                    Options {
                        contents: Contents::SymbolicLink,
                        ..Options::DEFAULT
                    },
                    &["/a.txt"],
                    "/b.txt"
                ),
                &fs,
                &out
            ),
            Ok(())
        );
        assert_eq!(fs.target_of("/b.txt").as_deref(), Some("/a.txt"));
        assert!(fs.contents("/b.txt").is_none());
    }

    // --- -P/-d: reproduce a link rather than following it ----------------

    #[test]
    fn without_no_dereference_a_link_source_is_followed_and_its_target_copied() {
        // The default: a copy of a link to a file is a copy of the file.
        let fs = MemFs::new()
            .file("/a.txt", b"hello")
            .symlink_at("/alias", "/a.txt");
        let out = Recorder::new();
        assert_eq!(
            run(copy(false, false, &["/alias"], "/b.txt"), &fs, &out),
            Ok(())
        );
        assert_eq!(fs.contents("/b.txt"), Some(b"hello".to_vec()));
        assert!(fs.target_of("/b.txt").is_none(), "not reproduced as a link");
    }

    #[test]
    fn no_dereference_reproduces_the_link_with_the_same_stored_target() {
        // Verbatim: a relative target stays relative rather than being
        // resolved against the source's directory.
        let fs = MemFs::new()
            .file("/a.txt", b"hello")
            .symlink_at("/alias", "../elsewhere/a.txt");
        let out = Recorder::new();
        assert_eq!(
            run(
                copy_with(
                    Options {
                        dereference: Dereference::Never,
                        ..Options::DEFAULT
                    },
                    &["/alias"],
                    "/b"
                ),
                &fs,
                &out
            ),
            Ok(())
        );
        assert_eq!(fs.target_of("/b").as_deref(), Some("../elsewhere/a.txt"));
        assert!(fs.contents("/b").is_none());
    }

    #[test]
    fn no_dereference_reproduces_a_dangling_link_too() {
        // A link's target is data, so a link naming nothing is copyable —
        // following it would be `NotFound`.
        let fs = MemFs::new().symlink_at("/alias", "nowhere");
        let out = Recorder::new();
        assert_eq!(
            run(
                copy_with(
                    Options {
                        dereference: Dereference::Never,
                        ..Options::DEFAULT
                    },
                    &["/alias"],
                    "/b"
                ),
                &fs,
                &out
            ),
            Ok(())
        );
        assert_eq!(fs.target_of("/b").as_deref(), Some("nowhere"));
        // And the default posture cannot copy it at all.
        let fs = MemFs::new().symlink_at("/alias", "nowhere");
        assert_eq!(
            run(copy(false, false, &["/alias"], "/c"), &fs, &Recorder::new()),
            Err(stat_error("/alias", Errno::NotFound))
        );
    }

    #[test]
    fn a_recursive_copy_reproduces_interior_links_under_no_dereference() {
        let fs = MemFs::new()
            .dir("/src")
            .file("/src/real", b"bytes")
            .symlink_at("/src/alias", "real");
        let out = Recorder::new();
        assert_eq!(
            run(
                copy_with(
                    Options {
                        recursive: true,
                        dereference: Dereference::Never,
                        ..Options::DEFAULT
                    },
                    &["/src"],
                    "/dst"
                ),
                &fs,
                &out
            ),
            Ok(())
        );
        assert_eq!(fs.contents("/dst/real"), Some(b"bytes".to_vec()));
        assert_eq!(fs.target_of("/dst/alias").as_deref(), Some("real"));
    }

    #[test]
    fn a_recursive_copy_reproduces_its_links_unless_told_to_follow_them() {
        // GNU's default under `-r`: a link inside the tree, and the operand
        // itself, are copied as links, so a link to `/` copies one link.
        let tree = || {
            MemFs::new()
                .dir("/src")
                .file("/src/real", b"bytes")
                .symlink_at("/src/alias", "/src/real")
                .symlink_at("/link", "/src")
                .symlink_at("/flink", "/src/real")
        };
        let fs = tree();
        assert_eq!(
            run(copy(true, false, &["/src"], "/dst"), &fs, &Recorder::new()),
            Ok(())
        );
        assert_eq!(fs.target_of("/dst/alias").as_deref(), Some("/src/real"));
        assert!(fs.contents("/dst/alias").is_none());
        assert_eq!(
            run(
                copy(true, false, &["/link"], "/lcopy"),
                &fs,
                &Recorder::new()
            ),
            Ok(())
        );
        assert_eq!(fs.target_of("/lcopy").as_deref(), Some("/src"));

        let recursive = |dereference| Options {
            recursive: true,
            dereference,
            ..Options::DEFAULT
        };
        // `-L` follows every link.
        let fs = tree().dir("/out");
        let all = copy_with(recursive(Dereference::Always), &["/src", "/flink"], "/out");
        assert_eq!(run(all, &fs, &Recorder::new()), Ok(()));
        assert_eq!(fs.contents("/out/src/alias"), Some(b"bytes".to_vec()));
        assert_eq!(fs.contents("/out/flink"), Some(b"bytes".to_vec()));
        // `-H` follows the operands' links but reproduces the ones inside.
        let fs = tree().dir("/out");
        let operands = copy_with(
            recursive(Dereference::Operands),
            &["/src", "/flink"],
            "/out",
        );
        assert_eq!(run(operands, &fs, &Recorder::new()), Ok(()));
        assert_eq!(fs.contents("/out/flink"), Some(b"bytes".to_vec()));
        assert_eq!(fs.target_of("/out/src/alias").as_deref(), Some("/src/real"));
        // `-l` links what each link names, so it follows them too.
        let fs = tree();
        let linked = Options {
            contents: Contents::HardLink,
            ..recursive(Dereference::Default)
        };
        assert_eq!(
            run(copy_with(linked, &["/src"], "/dst"), &fs, &Recorder::new()),
            Ok(())
        );
        assert_eq!(fs.object_of("/dst/alias").as_deref(), Some("/src/real"));
    }

    #[test]
    fn a_hard_link_to_a_followed_link_names_the_file_it_names() {
        let fs = MemFs::new()
            .dir("/src")
            .file("/src/real", b"bytes")
            .symlink_at("/flink", "/src/real");
        let link = Options {
            contents: Contents::HardLink,
            ..Options::DEFAULT
        };
        assert_eq!(
            run(copy_with(link, &["/flink"], "/x"), &fs, &Recorder::new()),
            Ok(())
        );
        assert!(
            fs.target_of("/x").is_none(),
            "a second name for the file, not the link"
        );
        assert_eq!(fs.object_of("/x").as_deref(), Some("/src/real"));
    }

    // --- a directory copied into itself -----------------------------------

    #[test]
    fn a_directory_copy_into_its_own_subtree_is_refused_before_anything_is_written() {
        let fs = MemFs::new().dir("/a").dir("/a/x").file("/a/f", b"bytes");
        let out = Recorder::new();
        assert_eq!(
            run(copy(true, false, &["/a"], "/a/b"), &fs, &out),
            Err(CpError::IntoItself {
                source: String::from("/a"),
                target: String::from("/a/b"),
            })
        );
        assert!(!fs.has_dir("/a/b"), "nothing was made");
        // Into an existing directory of its own, the copy would land at
        // `/a/x/a`, which is inside it too.
        assert_eq!(
            run(copy(true, false, &["/a"], "/a/x"), &fs, &out),
            Err(CpError::IntoItself {
                source: String::from("/a"),
                target: String::from("/a/x/a"),
            })
        );
        assert!(!fs.has_dir("/a/x/a"));
    }

    #[test]
    fn a_link_on_the_way_does_not_hide_the_source() {
        let fs = MemFs::new()
            .dir("/a")
            .dir("/b")
            .file("/a/f", b"bytes")
            .symlink_at("/b/to-a", "/a");
        assert_eq!(
            run(
                copy(true, false, &["/a"], "/b/to-a/c"),
                &fs,
                &Recorder::new()
            ),
            Err(CpError::IntoItself {
                source: String::from("/a"),
                target: String::from("/b/to-a/c"),
            })
        );
        assert!(!fs.has_dir("/a/c"));
    }

    #[test]
    fn a_directory_copied_onto_itself_is_the_same_file() {
        let fs = MemFs::new().dir("/a").file("/a/f", b"bytes");
        let onto = Options {
            recursive: true,
            target_mode: TargetMode::NoDirectory,
            ..Options::DEFAULT
        };
        assert_eq!(
            run(copy_with(onto, &["/a"], "/a"), &fs, &Recorder::new()),
            Err(CpError::SameFile {
                source: String::from("/a"),
                target: String::from("/a"),
            })
        );
    }

    #[test]
    fn a_walk_that_meets_a_directory_it_is_inside_stops_there() {
        // Under `-L` the loop's link leads the walk back into `/a`, a
        // directory it is already copying.
        let fs = MemFs::new()
            .dir("/a")
            .file("/a/f", b"bytes")
            .symlink_at("/a/loop", "/a");
        let follow = Options {
            recursive: true,
            dereference: Dereference::Always,
            ..Options::DEFAULT
        };
        assert_eq!(
            run(copy_with(follow, &["/a"], "/dst"), &fs, &Recorder::new()),
            Err(CpError::IntoItself {
                source: String::from("/a/loop"),
                target: String::from("/dst/loop"),
            })
        );
        assert!(!fs.has_dir("/dst/loop"));
    }

    // --- --preserve=links: keep the sources' sharing ----------------------

    #[test]
    fn preserve_links_gives_a_second_source_naming_one_node_a_second_name() {
        // Two sources, one node: the destinations share a node too, so the
        // copy does not silently double the storage.
        let fs = MemFs::new()
            .dir("/dst")
            .file("/one", b"shared")
            .second_name("/two", "/one");
        let out = Recorder::new();
        assert_eq!(
            run(
                copy_with(
                    Options {
                        preserve_links: true,
                        ..Options::DEFAULT
                    },
                    &["/one", "/two"],
                    "/dst"
                ),
                &fs,
                &out
            ),
            Ok(())
        );
        assert_eq!(fs.contents("/dst/one"), Some(b"shared".to_vec()));
        assert!(fs.is_second_name_of("/dst/two", "/dst/one"));
        assert!(
            fs.contents("/dst/two").is_none(),
            "the second name is not a second copy"
        );
    }

    /// A kept destination is no copy of the node: linking the node's next
    /// name to it would give that name an unrelated file's bytes.
    #[test]
    fn a_destination_kept_at_the_question_is_never_linked_to_the_nodes_next_name() {
        let fs = MemFs::new()
            .dir("/dst")
            .file("/dst/one", b"unrelated")
            .file("/one", b"shared")
            .second_name("/two", "/one");
        let out = Recorder::new();
        let prompt = Answers::new(&[false]);
        let options = Options {
            preserve_links: true,
            clobber: Clobber::Prompt,
            ..Options::DEFAULT
        };
        assert_eq!(
            engine_run(
                copy_with(options, &["/one", "/two"], "/dst"),
                None,
                seams(&fs, &prompt, &NoHelp, &out, &out),
            ),
            Ok(())
        );
        assert_eq!(fs.contents("/dst/one"), Some(b"unrelated".to_vec()));
        assert_eq!(fs.contents("/dst/two"), Some(b"shared".to_vec()));
        assert!(!fs.is_second_name_of("/dst/two", "/dst/one"));
    }

    /// GNU 9.4's refusal, and its wording.
    #[test]
    fn an_operand_never_replaces_what_an_earlier_one_copied_into_place() {
        let fs = MemFs::new()
            .dir("/d")
            .file("/a/x", b"first")
            .file("/b/x", b"second")
            .file("/b/y", b"third");
        let out = Recorder::new();
        assert_eq!(
            run(
                copy(false, false, &["/a/x", "/b/x", "/b/y"], "/d"),
                &fs,
                &out
            ),
            Err(CpError::JustCreated {
                source: String::from("/b/x"),
                target: String::from("/d/x"),
            })
        );
        assert_eq!(fs.contents("/d/x"), Some(b"first".to_vec()));
        let err = CpError::JustCreated {
            source: String::from("b/x"),
            target: String::from("e/x"),
        };
        assert_eq!(
            format!("{err}"),
            "will not overwrite just-created 'e/x' with 'b/x'"
        );
        // A directory merges into one an earlier operand made, as GNU's does.
        let fs = MemFs::new()
            .dir("/g")
            .dir("/f/sub")
            .file("/f/sub/z", b"4")
            .dir("/h/sub")
            .file("/h/sub/z", b"5");
        assert_eq!(
            run(copy(true, false, &["/f/sub", "/h/sub"], "/g"), &fs, &out),
            Ok(())
        );
        assert_eq!(fs.contents("/g/sub/z"), Some(b"5".to_vec()));
    }

    #[test]
    fn without_preserve_links_each_name_is_copied_separately() {
        let fs = MemFs::new()
            .dir("/dst")
            .file("/one", b"shared")
            .second_name("/two", "/one");
        let out = Recorder::new();
        assert_eq!(
            run(copy(false, false, &["/one", "/two"], "/dst"), &fs, &out),
            Ok(())
        );
        assert_eq!(fs.contents("/dst/one"), Some(b"shared".to_vec()));
        assert_eq!(fs.contents("/dst/two"), Some(b"shared".to_vec()));
        assert!(!fs.is_second_name_of("/dst/two", "/dst/one"));
    }

    #[test]
    fn preserve_links_leaves_singly_named_sources_alone() {
        // A node named once cannot be met twice, so nothing is remembered
        // for it and two distinct files stay two files.
        let fs = MemFs::new()
            .dir("/dst")
            .file("/one", b"a")
            .file("/two", b"b");
        let out = Recorder::new();
        assert_eq!(
            run(
                copy_with(
                    Options {
                        preserve_links: true,
                        ..Options::DEFAULT
                    },
                    &["/one", "/two"],
                    "/dst"
                ),
                &fs,
                &out
            ),
            Ok(())
        );
        assert_eq!(fs.contents("/dst/one"), Some(b"a".to_vec()));
        assert_eq!(fs.contents("/dst/two"), Some(b"b".to_vec()));
        assert!(!fs.is_second_name_of("/dst/two", "/dst/one"));
    }

    #[test]
    fn help_writes_usage() {
        let fs = MemFs::new();
        let out = Recorder::new();
        assert_eq!(run(Command::Help, &fs, &out), Ok(()));
        assert_eq!(out.text(), USAGE);
    }

    #[test]
    fn help_renders_the_short_help_from_the_document() {
        let fs = MemFs::new();
        let out = Recorder::new();
        assert_eq!(
            engine_run(
                Command::Help,
                None,
                seams(&fs, &NeverAsked, &OneDoc, &out, &out)
            ),
            Ok(())
        );
        let text = out.text();
        assert!(text.contains("cp — copy files and directories"), "{text}");
        assert!(text.contains("cp [-r] [--] source... dest"), "{text}");
    }

    #[test]
    fn copies_a_single_file_to_a_new_path() {
        let fs = MemFs::new().file("/a.txt", b"hello world");
        let out = Recorder::new();
        assert_eq!(
            run(copy(false, false, &["/a.txt"], "/b.txt"), &fs, &out),
            Ok(())
        );
        assert_eq!(fs.contents("/b.txt").as_deref(), Some(&b"hello world"[..]));
        // The source is untouched, and `cp` is silent on success.
        assert_eq!(fs.contents("/a.txt").as_deref(), Some(&b"hello world"[..]));
        assert_eq!(out.text(), "");
    }

    #[test]
    fn copies_a_file_across_the_chunk_boundary() {
        // A payload larger than one chunk exercises the streaming loop.
        let payload: Vec<u8> = (0..(tairix_fscopy::CHUNK_LEN * 2 + 7))
            .map(|i| u8::try_from(i % 251).unwrap_or_default())
            .collect();
        let fs = MemFs::new().file("/big", &payload);
        let out = Recorder::new();
        assert_eq!(
            run(copy(false, false, &["/big"], "/copy"), &fs, &out),
            Ok(())
        );
        assert_eq!(fs.contents("/copy"), Some(payload));
    }

    #[test]
    fn copies_an_empty_file() {
        let fs = MemFs::new().file("/empty", b"");
        let out = Recorder::new();
        assert_eq!(
            run(copy(false, false, &["/empty"], "/dup"), &fs, &out),
            Ok(())
        );
        assert_eq!(fs.contents("/dup").as_deref(), Some(&b""[..]));
    }

    #[test]
    fn copies_a_file_into_an_existing_directory_under_its_basename() {
        let fs = MemFs::new().file("/src/a.txt", b"data").dir("/dst");
        let out = Recorder::new();
        assert_eq!(
            run(copy(false, false, &["/src/a.txt"], "/dst"), &fs, &out),
            Ok(())
        );
        assert_eq!(fs.contents("/dst/a.txt").as_deref(), Some(&b"data"[..]));
    }

    #[test]
    fn copies_several_files_into_a_directory() {
        let fs = MemFs::new()
            .file("/a", b"one")
            .file("/b", b"two")
            .dir("/dst");
        let out = Recorder::new();
        assert_eq!(
            run(copy(false, false, &["/a", "/b"], "/dst"), &fs, &out),
            Ok(())
        );
        assert_eq!(fs.contents("/dst/a").as_deref(), Some(&b"one"[..]));
        assert_eq!(fs.contents("/dst/b").as_deref(), Some(&b"two"[..]));
    }

    #[test]
    fn several_sources_to_a_non_directory_dest_is_usage() {
        let fs = MemFs::new().file("/a", b"x").file("/b", b"y");
        let out = Recorder::new();
        assert_eq!(
            run(copy(false, false, &["/a", "/b"], "/c"), &fs, &out),
            Err(CpError::Usage)
        );
        // Nothing was created.
        assert!(fs.contents("/c").is_none());
    }

    #[test]
    fn a_directory_source_without_recursive_fails_closed() {
        let fs = MemFs::new().dir("/d");
        let out = Recorder::new();
        assert_eq!(
            run(copy(false, false, &["/d"], "/e"), &fs, &out),
            Err(CpError::IsDirectory)
        );
        assert!(!fs.has_dir("/e"));
    }

    #[test]
    fn recursive_reproduces_a_subtree() {
        // /d holds a file and a nested directory with its own file.
        let fs = MemFs::new()
            .dir("/d")
            .file("/d/f", b"top")
            .dir("/d/sub")
            .file("/d/sub/g", b"nested");
        let out = Recorder::new();
        assert_eq!(run(copy(true, false, &["/d"], "/e"), &fs, &out), Ok(()));
        assert!(fs.has_dir("/e"));
        assert!(fs.has_dir("/e/sub"));
        assert_eq!(fs.contents("/e/f").as_deref(), Some(&b"top"[..]));
        assert_eq!(fs.contents("/e/sub/g").as_deref(), Some(&b"nested"[..]));
    }

    #[test]
    fn recursive_into_an_existing_directory_merges() {
        let fs = MemFs::new().dir("/d").file("/d/f", b"x").dir("/dst");
        let out = Recorder::new();
        // The destination already exists, so /d is reproduced as /dst/d.
        assert_eq!(run(copy(true, false, &["/d"], "/dst"), &fs, &out), Ok(()));
        assert!(fs.has_dir("/dst/d"));
        assert_eq!(fs.contents("/dst/d/f").as_deref(), Some(&b"x"[..]));
    }

    #[test]
    fn recursive_onto_an_existing_file_fails_closed() {
        let fs = MemFs::new()
            .dir("/d")
            .file("/d/f", b"x")
            .file("/e", b"blocker");
        let out = Recorder::new();
        assert_eq!(
            run(copy(true, false, &["/d"], "/e"), &fs, &out),
            Err(CpError::NotADirectory)
        );
    }

    #[test]
    fn a_missing_source_fails_closed() {
        let fs = MemFs::new();
        let out = Recorder::new();
        assert_eq!(
            run(copy(false, false, &["/absent"], "/dst"), &fs, &out),
            Err(stat_error("/absent", Errno::NotFound))
        );
    }

    #[test]
    fn a_failure_stops_before_a_later_source() {
        let fs = MemFs::new().file("/b", b"present").dir("/dst");
        let out = Recorder::new();
        // The first source is missing, so the second is never copied.
        assert_eq!(
            run(copy(false, false, &["/absent", "/b"], "/dst"), &fs, &out),
            Err(stat_error("/absent", Errno::NotFound))
        );
        assert!(fs.contents("/dst/b").is_none());
    }

    #[test]
    fn an_unreadable_source_surfaces_the_errno() {
        let fs = MemFs::new()
            .file("/a", b"data")
            .read_fails("/a", Errno::PermissionDenied);
        let out = Recorder::new();
        assert_eq!(
            run(copy(false, false, &["/a"], "/b"), &fs, &out),
            Err(read_error("/a", Errno::PermissionDenied))
        );
    }

    #[test]
    fn an_uncreatable_destination_surfaces_the_errno() {
        let fs = MemFs::new()
            .file("/a", b"data")
            .create_fails("/b", Errno::PermissionDenied);
        let out = Recorder::new();
        assert_eq!(
            run(copy(false, false, &["/a"], "/b"), &fs, &out),
            Err(create_error("/b", Errno::PermissionDenied))
        );
    }

    #[test]
    fn force_removes_an_unwritable_destination_and_retries() {
        let fs = MemFs::new()
            .file("/a", b"fresh")
            .file("/b", b"stale")
            .create_fails("/b", Errno::PermissionDenied);
        let out = Recorder::new();
        // Without -f this would be a Create error; -f removes /b and retries.
        assert_eq!(run(copy(false, true, &["/a"], "/b"), &fs, &out), Ok(()));
        assert_eq!(fs.removed(), ["/b"]);
        assert_eq!(fs.contents("/b").as_deref(), Some(&b"fresh"[..]));
    }

    #[test]
    fn a_failure_names_the_path_it_met_quoted() {
        let fs = MemFs::new()
            .file("/a", b"data")
            .create_fails("/b\u{1b}", Errno::PermissionDenied);
        let err = run(
            copy(false, false, &["/a"], "/b\u{1b}"),
            &fs,
            &Recorder::new(),
        )
        .expect_err("the create is refused");
        assert_eq!(
            format!("{err}"),
            format!("cannot create '/b'$'\\033': {}", Errno::PermissionDenied)
        );
    }

    #[test]
    fn a_failed_write_surfaces_the_errno() {
        let fs = MemFs::new()
            .file("/a", b"data")
            .write_fails("/b", Errno::PermissionDenied);
        let out = Recorder::new();
        assert_eq!(
            run(copy(false, false, &["/a"], "/b"), &fs, &out),
            Err(write_error("/b", Errno::PermissionDenied))
        );
    }

    #[test]
    fn help_output_failure_propagates() {
        let fs = MemFs::new();
        let out = Recorder::failing();
        assert_eq!(
            run(Command::Help, &fs, &out),
            Err(CpError::Output(Errno::NotFound))
        );
    }

    #[test]
    fn a_trailing_slash_source_copies_under_its_basename() {
        // `cp -r /d/ /dst` copies into /dst/d, not /dst//.
        let fs = MemFs::new().dir("/d").file("/d/f", b"x").dir("/dst");
        let out = Recorder::new();
        assert_eq!(run(copy(true, false, &["/d/"], "/dst"), &fs, &out), Ok(()));
        assert!(fs.has_dir("/dst/d"));
        assert_eq!(fs.contents("/dst/d/f").as_deref(), Some(&b"x"[..]));
    }

    #[test]
    fn an_alias_root_source_lands_under_its_own_root_spelling() {
        // Behaviour moved with the sweep onto the shared `leaf_name`: the
        // private base name this tool carried was not alias-aware and named
        // `Home:/` as `Home:`, dropping the separator that makes it a root.
        assert_eq!(tairix_path::leaf_name("Home:/"), "Home:/");
        // And an ordinary path is unchanged by the move.
        assert_eq!(tairix_path::leaf_name("/a/b/"), "b");
    }

    #[test]
    fn an_empty_source_operand_names_nothing_and_fails_closed() {
        // The other moved answer: the private base name reported `/` for the
        // empty spelling, fabricating a root component the caller never
        // typed. `leaf_name` names nothing, and the copy still fails on the
        // unreadable source rather than acting on a guessed path.
        assert_eq!(tairix_path::leaf_name(""), "");
        let fs = MemFs::new().dir("/dst");
        let out = Recorder::new();
        assert!(run(copy(false, false, &[""], "/dst"), &fs, &out).is_err());
        assert!(!fs.has_dir("/dst/"));
    }

    #[test]
    fn no_clobber_skips_an_existing_destination() {
        let fs = MemFs::new().file("/a", b"fresh").file("/b", b"stale");
        let out = Recorder::new();
        assert_eq!(
            run(
                copy_with(
                    Options {
                        clobber: Clobber::Skip,
                        ..Options::DEFAULT
                    },
                    &["/a"],
                    "/b",
                ),
                &fs,
                &out,
            ),
            Ok(())
        );
        // The destination keeps its bytes; nothing was copied.
        assert_eq!(fs.contents("/b").as_deref(), Some(&b"stale"[..]));
    }

    #[test]
    fn no_clobber_still_copies_to_a_new_destination() {
        let fs = MemFs::new().file("/a", b"fresh");
        let out = Recorder::new();
        assert_eq!(
            run(
                copy_with(
                    Options {
                        clobber: Clobber::Skip,
                        ..Options::DEFAULT
                    },
                    &["/a"],
                    "/b",
                ),
                &fs,
                &out,
            ),
            Ok(())
        );
        assert_eq!(fs.contents("/b").as_deref(), Some(&b"fresh"[..]));
    }

    #[test]
    fn interactive_asks_before_overwriting() {
        let fs = MemFs::new()
            .file("/a", b"fresh")
            .file("/b", b"stale")
            .file("/c", b"old")
            .dir("/dst")
            .file("/dst/a", b"blockA")
            .file("/dst/c", b"blockC");
        let out = Recorder::new();
        let prompt = Answers::new(&[true, false]);
        assert_eq!(
            engine_run(
                copy_with(
                    Options {
                        clobber: Clobber::Prompt,
                        ..Options::DEFAULT
                    },
                    &["/a", "/c"],
                    "/dst",
                ),
                None,
                seams(&fs, &prompt, &NoHelp, &out, &out),
            ),
            Ok(())
        );
        // `/dst/a` was confirmed and overwritten; `/dst/c` was declined and
        // kept, and the run still succeeds.
        assert_eq!(fs.contents("/dst/a").as_deref(), Some(&b"fresh"[..]));
        assert_eq!(fs.contents("/dst/c").as_deref(), Some(&b"blockC"[..]));
        assert_eq!(
            prompt.asked(),
            ["overwrite '/dst/a'?", "overwrite '/dst/c'?"]
        );
    }

    #[test]
    fn an_unanswerable_prompt_fails_closed() {
        let fs = MemFs::new().file("/a", b"fresh").file("/b", b"stale");
        let out = Recorder::new();
        let prompt = Answers::new(&[]);
        assert_eq!(
            engine_run(
                copy_with(
                    Options {
                        clobber: Clobber::Prompt,
                        ..Options::DEFAULT
                    },
                    &["/a"],
                    "/b",
                ),
                None,
                seams(&fs, &prompt, &NoHelp, &out, &out),
            ),
            Err(CpError::Prompt(Errno::NotFound))
        );
        assert_eq!(fs.contents("/b").as_deref(), Some(&b"stale"[..]));
    }

    #[test]
    fn verbose_reports_each_copy_in_gnu_wording() {
        let fs = MemFs::new().dir("/d").file("/d/f", b"x").dir("/dst");
        let out = Recorder::new();
        assert_eq!(
            run(
                copy_with(
                    Options {
                        recursive: true,
                        verbose: true,
                        ..Options::DEFAULT
                    },
                    &["/d"],
                    "/dst",
                ),
                &fs,
                &out,
            ),
            Ok(())
        );
        assert_eq!(out.text(), "'/d' -> '/dst/d'\n'/d/f' -> '/dst/d/f'\n");
    }

    #[test]
    fn target_directory_copies_every_source_into_it() {
        let fs = MemFs::new()
            .file("/a", b"one")
            .file("/b", b"two")
            .dir("/dst");
        let out = Recorder::new();
        assert_eq!(
            run(
                copy_with(
                    Options {
                        target_mode: TargetMode::Directory,
                        ..Options::DEFAULT
                    },
                    &["/a", "/b"],
                    "/dst",
                ),
                &fs,
                &out,
            ),
            Ok(())
        );
        assert_eq!(fs.contents("/dst/a").as_deref(), Some(&b"one"[..]));
        assert_eq!(fs.contents("/dst/b").as_deref(), Some(&b"two"[..]));
    }

    #[test]
    fn target_directory_must_exist_and_be_a_directory() {
        let fs = MemFs::new().file("/a", b"one").file("/blocker", b"x");
        let out = Recorder::new();
        assert_eq!(
            run(
                copy_with(
                    Options {
                        target_mode: TargetMode::Directory,
                        ..Options::DEFAULT
                    },
                    &["/a"],
                    "/blocker",
                ),
                &fs,
                &out,
            ),
            Err(CpError::NotADirectory)
        );
        assert_eq!(
            run(
                copy_with(
                    Options {
                        target_mode: TargetMode::Directory,
                        ..Options::DEFAULT
                    },
                    &["/a"],
                    "/absent",
                ),
                &fs,
                &out,
            ),
            Err(stat_error("/absent", Errno::NotFound))
        );
    }

    #[test]
    fn no_target_directory_refuses_several_sources() {
        let fs = MemFs::new().file("/a", b"x").file("/b", b"y").dir("/dst");
        let out = Recorder::new();
        assert_eq!(
            run(
                copy_with(
                    Options {
                        target_mode: TargetMode::NoDirectory,
                        ..Options::DEFAULT
                    },
                    &["/a", "/b"],
                    "/dst",
                ),
                &fs,
                &out,
            ),
            Err(CpError::Usage)
        );
    }
}
