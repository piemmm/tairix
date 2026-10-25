//! The copy engine: resolve each source's destination, copy regular files
//! through the shared engine with their holes and extended attributes, and
//! reproduce each directory `-r` must copy through the shared walk — each
//! entry reached by name from a source directory held open, and copied by
//! name into a destination directory the copy made or opened, a directory it
//! made able to take its entries until they are in and then given the mode
//! GNU's `cp` gives it.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::fs::{FS_NEW_DIR_MODE, FS_NEW_FILE_MODE};
use tairix_abi::{Errno, FileId, FileStat, NodeExpect, UnlinkFlags};
use tairix_collections::{HashMap, HashSet};
use tairix_fscopy::tree::{entry_name, unlink_node, walk, Entry, Step, Visitor};
use tairix_fscopy::{
    copy_attributes, file_mode, landing, AttrError, Created, DataCopy, DataError, Descent,
    DirModes, Landing, Omissions, Settle, CHUNK_LEN,
};
use tairix_hash::BuildSipHash13;
use tairix_help::{own_short_help, HelpSource};
use tairix_path::{dir_name, join, leaf_name};
use tairix_util::output::Carried;
use tairix_util::quote::quote;

use crate::command::{Clobber, Command, Contents, Dereference, Options, TargetMode};
use crate::error::CpError;
use crate::io::{Dir, FileSystem, FinalLink, Kind, Node, Output, Probe, Prompt, Roots};

/// The usage banner the short-help switches print when `cp`'s own Help tree
/// is unavailable.
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
  --no-preserve=mode         make each new file in the default mode, not
                             its source's
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
/// A failed operand is reported on the diagnostics stream in GNU's words and
/// the next is still copied, as GNU's are, answering `Ok(false)` at the end;
/// every operand copied answers `Ok(true)`. Only a
/// [fatal](tairix_util::output::Failure::is_fatal) failure is returned.
///
/// # Errors
///
/// * [`CpError::Usage`] — more than one source under `-T`.
/// * [`CpError::Target`] — the `-t` operand, or the destination of several
///   sources, is missing or not a directory.
/// * [`CpError::MemoryExhausted`] — a record of the run could not grow.
/// * [`CpError::Prompt`] — a confirmation could not be read (never treated
///   as consent).
/// * [`CpError::Output`] — writing the usage banner, a `-v` report or a
///   diagnostic failed.
///
/// The rest are reported per operand: [`CpError::IsDirectory`] — a directory
/// source named without `-r`; [`CpError::Overwrite`] — a directory source
/// whose destination exists as a non-directory; [`CpError::Stat`],
/// [`CpError::Read`], [`CpError::Create`], [`CpError::Write`] — a path that
/// could not be inspected, read, made or written; [`CpError::Attribute`] — an
/// extended attribute the destination refused and the copy had to carry;
/// [`CpError::SameFile`], [`CpError::IntoItself`], [`CpError::JustCreated`] —
/// a copy that would empty, nest in or replace what it copies.
///
/// Attributes a destination could not keep are reported once, after the
/// last copy and before any failure that ended the run: a warning on the
/// diagnostics stream and an advisory record on the information stream.
pub fn run<F: FileSystem>(
    command: Command,
    locale: Option<&str>,
    seams: &Seams<'_, F>,
) -> Result<bool, CpError> {
    match command {
        Command::Help => short_help(locale, seams.help, seams.out).map(|()| true),
        Command::Copy {
            options,
            sources,
            dest,
        } => {
            let mut copier = Copier {
                options,
                linked: Linked::new(seams.keys),
                sources: Sources::new(seams.keys),
                created: Created::new(seams.keys),
                omissions: Omissions::default(),
                descent: Descent::default(),
                buf: Vec::new(),
                fs: seams.fs,
                prompt: seams.prompt,
                out: seams.out,
                diagnostics: seams.diagnostics,
                carried: Carried::new(seams.diagnostics, OWN_WORD),
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
pub struct Seams<'a, F: FileSystem> {
    /// The files copied.
    pub fs: &'a F,
    /// The `-i` question.
    pub prompt: &'a dyn Prompt,
    /// The tool's own `Help/` tree, read by the short-help switches.
    pub help: &'a dyn HelpSource,
    /// The `-v` reports, the usage banner and the information stream.
    pub out: &'a dyn Output,
    /// The warnings: a source named twice, attributes left behind.
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
struct Copier<'a, F: FileSystem> {
    options: Options,
    /// The destinations already written for each multiply-named source node
    /// (`--preserve=links`).
    linked: Linked,
    /// The sources earlier operands named.
    sources: Sources,
    /// The destinations earlier operands put in place.
    created: Created,
    /// What the copies' destinations could not keep, reported once.
    omissions: Omissions,
    /// The directories the walk is inside, so it never enters one again.
    descent: Descent,
    /// The one buffer every file's bytes pass through, sized on first use.
    buf: Vec<u8>,
    fs: &'a F,
    prompt: &'a dyn Prompt,
    out: &'a dyn Output,
    diagnostics: &'a dyn Output,
    /// The failures the run reports and carries past.
    carried: Carried<'a>,
}

/// The sources earlier operands named, so a later operand naming one again —
/// `cp f ./f dir/` — is skipped with GNU's warning rather than refused as
/// just-created.
///
/// One node reached through two entries — a link and what it leads to, or two
/// hard links — is two sources, as GNU copies it. A node's first operand is
/// kept as typed and its entry resolved only once a second operand reaches the
/// node, so a run of distinct sources pays nothing.
struct Sources {
    nodes: HashMap<FileId, Reached, BuildSipHash13>,
    /// The entries named so far, for the nodes more than one operand reached.
    places: HashSet<Place, BuildSipHash13>,
}

/// What a run knows of the operands that reached one node.
enum Reached {
    /// One, spelt so.
    Once(String),
    /// Several, whose entries [`Sources::places`] holds.
    Several,
}

/// The directory entry an operand names: the node it reached, the directory
/// holding the entry and the entry's own name there.
#[derive(Debug, Eq, Hash, PartialEq)]
struct Place {
    node: FileId,
    directory: FileId,
    name: String,
}

impl Sources {
    const fn new(keys: BuildSipHash13) -> Self {
        Self {
            nodes: HashMap::with_hasher(keys),
            places: HashSet::with_hasher(keys),
        }
    }

    /// Whether `source`, whose node is `node`, names an entry an earlier
    /// operand named, remembering it otherwise.
    ///
    /// # Errors
    ///
    /// [`CpError::Stat`] when an entry's directory cannot be opened, and
    /// [`CpError::MemoryExhausted`] when the record cannot grow.
    fn repeats<R: Roots>(&mut self, fs: &R, node: FileId, source: &str) -> Result<bool, CpError> {
        if node.is_none() {
            return Ok(false);
        }
        let grown = |_| CpError::MemoryExhausted;
        match self.nodes.get_mut(&node) {
            None => {
                self.nodes
                    .try_insert(node, Reached::Once(String::from(source)))
                    .map_err(grown)?;
                return Ok(false);
            }
            Some(reached) => {
                if let Reached::Once(first) = reached {
                    let place = place(fs, node, first)?;
                    self.places.try_insert(place).map_err(grown)?;
                    *reached = Reached::Several;
                }
            }
        }
        let place = place(fs, node, source)?;
        Ok(!self.places.try_insert(place).map_err(grown)?)
    }
}

/// The entry `source`, whose node is `node`, names: the directory holding it,
/// opened through the path as written, and its own name unfollowed, so `f`,
/// `./f` and `f/` are one entry while a link and what it leads to are two. A
/// root, `.` or `..` is no entry of its own and stands for its node.
fn place<R: Roots>(fs: &R, node: FileId, source: &str) -> Result<Place, CpError> {
    if entry_name(source).is_none() {
        return Ok(Place {
            node,
            directory: node,
            name: String::new(),
        });
    }
    let (dir, name) = fs
        .parent(source)
        .map_err(|errno| stat_error(source, errno))?;
    let directory = dir
        .stat()
        .map_err(|errno| stat_error(dir_name(source), errno))?
        .id;
    Ok(Place {
        node,
        directory,
        name,
    })
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
    /// The destination first written for each source node, and the node the
    /// run made there.
    seen: HashMap<FileId, (String, FileId), BuildSipHash13>,
}

impl Linked {
    const fn new(keys: BuildSipHash13) -> Self {
        Self {
            seen: HashMap::with_hasher(keys),
        }
    }

    /// The destination already written for `probe`'s node and the node made
    /// there, if any.
    fn destination_of(&self, probe: &Probe) -> Option<(&str, FileId)> {
        if probe.nlink <= 1 || probe.id.is_none() {
            return None;
        }
        self.seen
            .get(&probe.id)
            .map(|(target, made)| (target.as_str(), *made))
    }

    /// Remember `target` as the destination written for `probe`'s node.
    ///
    /// A node that cannot recur, or one the backing offers no identity for,
    /// is not remembered; nor is anything, if the heap cannot grow the map.
    fn remember(&mut self, probe: &Probe, target: &str, made: FileId) {
        if probe.nlink <= 1
            || probe.id.is_none()
            || made.is_none()
            || self.seen.contains_key(&probe.id)
        {
            return;
        }
        let mut name = String::new();
        if name.try_reserve_exact(target.len()).is_err() {
            return;
        }
        name.push_str(target);
        let _ = self.seen.try_insert(probe.id, (name, made));
    }
}

/// Where a link may be put, once the existing-destination policy allows one.
#[derive(Clone, Copy, Eq, PartialEq)]
enum Slot {
    /// In the name as it is: vacant, or held by an occupant the create then
    /// meets.
    Free,
    /// In place of the occupant, removed only while it is this node.
    Taken(FileId),
}

/// What a link put on the very node it would name is, as GNU judges it.
#[derive(Clone, Copy)]
enum Onto {
    /// A second name (`-l`, `--preserve=links`): already made.
    Made,
    /// A symbolic link (`-s`, a link reproduced under `-P`): the same file,
    /// refused.
    Refused,
}

/// Where a copied node comes from: an operand's path, read through a final
/// link as the run's posture says, or a name in a source directory the walk
/// holds, never through a link.
enum Origin<'o, D> {
    Operand(&'o str),
    Held(&'o D, &'o str),
    /// A name in a held source directory, through its final link: a link the
    /// run follows inside a tree.
    Followed(&'o D, &'o str),
}

impl<D> Clone for Origin<'_, D> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<D> Copy for Origin<'_, D> {}

/// Where a copy's destination is: an operand's own path, written through a
/// final link as GNU's `cp` writes one, or a name in a destination directory
/// the copy holds, never through a link.
enum Spot<'s, D> {
    Operand(&'s str),
    Held(&'s D, &'s str),
}

impl<D> Clone for Spot<'_, D> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<D> Copy for Spot<'_, D> {}

impl<F: FileSystem> Copier<'_, F> {
    /// Copy every source to `dest`, deciding per source whether the
    /// destination is `dest` itself or a child of `dest` named after the
    /// source.
    fn copy_all(&mut self, sources: &[String], dest: &str) -> Result<bool, CpError> {
        // A destination is always probed through a final link: a directory
        // reached by a link is still the directory the copies land in, and
        // `-P`/`-d` are about how *sources* are read, never where output goes.
        let dest_kind = self.stat(dest, FinalLink::Follow)?.map(|p| p.kind);
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
                    return Err(CpError::Usage);
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
            let copied = self.copy_operand(source, dest, dest_is_dir);
            self.carried.absorb(copied)?;
        }
        Ok(self.carried.clean())
    }

    /// Copy `source` to `dest`, or into it when `dest_is_dir`.
    fn copy_operand(&mut self, source: &str, dest: &str, dest_is_dir: bool) -> Result<(), CpError> {
        let target = if dest_is_dir {
            join(dest, leaf_name(source))
        } else {
            String::from(dest)
        };
        let follow = self.operand_follow();
        let probe = self.probe(source, follow)?;
        if self.sources.repeats(self.fs, probe.id, source)? {
            let what = if probe.kind == Kind::Directory {
                "directory"
            } else {
                "file"
            };
            // The warning is advisory: a stream refusing it changes nothing.
            let _ = self.diagnostics.write_all(
                format!(
                    "{OWN_WORD}: warning: source {what} {} specified more than once\n",
                    quote(source)
                )
                .as_bytes(),
            );
            return Ok(());
        }
        if probe.kind == Kind::Directory && self.options.recursive {
            self.refuse_landing_in(source, follow, &target)?;
        }
        // A directory merges into one an earlier operand made, as GNU's
        // does; anything else would replace it.
        let guarded = probe.kind != Kind::Directory;
        let destination = self.destination_follow(&probe);
        if guarded {
            if let Some(there) = self.stat(&target, destination)? {
                if self.created.holds(there.id, &target) {
                    return Err(CpError::JustCreated {
                        source: String::from(source),
                        target,
                    });
                }
            }
        }
        let written = self.copy_known(
            Origin::Operand(source),
            source,
            probe,
            Spot::Operand(&target),
            &target,
        )?;
        if guarded && written {
            if let Some(made) = self.stat(&target, destination)? {
                self.created
                    .note(made.id, &target)
                    .map_err(|_| CpError::MemoryExhausted)?;
            }
        }
        Ok(())
    }

    /// How a destination for `probe` is looked at: through a final link for a
    /// byte copy, which writes what the link names, and as itself for a link
    /// or a second name, which replace the name.
    fn destination_follow(&self, probe: &Probe) -> FinalLink {
        if probe.kind != Kind::Link
            && probe.kind != Kind::Directory
            && self.options.contents == Contents::Bytes
        {
            FinalLink::Follow
        } else {
            FinalLink::Keep
        }
    }

    /// Refuse, before anything is written, a directory copy whose `target`
    /// is the directory `source` or would be made inside it: judged from the
    /// directories themselves, by climbing from where the copy would write.
    fn refuse_landing_in(
        &self,
        source: &str,
        follow: FinalLink,
        target: &str,
    ) -> Result<(), CpError> {
        let held = self
            .fs
            .open_dir(source, follow == FinalLink::Follow)
            .map_err(|errno| stat_error(source, errno))?;
        let (into, merging) = match self.fs.open_dir(target, true) {
            Ok(existing) => (existing, true),
            Err(Errno::NotFound | Errno::NotADirectory) => {
                let (parent, _) = self
                    .fs
                    .parent(target)
                    .map_err(|errno| stat_error(target, errno))?;
                (parent, false)
            }
            Err(errno) => return Err(stat_error(target, errno)),
        };
        let landed = landing(&held, &into).map_err(|unresolved| {
            let (path, errno) = unresolved.at(source, target);
            stat_error(path, errno)
        })?;
        let named = || (String::from(source), String::from(target));
        match landed {
            Landing::Apart => Ok(()),
            Landing::Same if merging => {
                let (source, target) = named();
                Err(CpError::SameFile { source, target })
            }
            Landing::Same | Landing::Within => {
                let (source, target) = named();
                Err(CpError::IntoItself { source, target })
            }
        }
    }

    /// Copy a node a look already described, from `origin` (known as
    /// `source`) to `spot` (known as `target`), recursing into a directory;
    /// whether the destination was written rather than kept.
    fn copy_known(
        &mut self,
        origin: Origin<'_, F::Dir>,
        source: &str,
        probe: Probe,
        spot: Spot<'_, F::Dir>,
        target: &str,
    ) -> Result<bool, CpError> {
        match probe.kind {
            Kind::Directory => {
                if !self.options.recursive {
                    return Err(CpError::IsDirectory(String::from(source)));
                }
                let held = match origin {
                    Origin::Operand(path) => self.fs.open_dir(path, true),
                    Origin::Held(dir, name) => dir.open_dir(name),
                    Origin::Followed(dir, name) => dir.enter(name),
                }
                .map_err(|errno| read_error(source, errno))?;
                self.reproduce_directory(&held, source, &probe, spot, target)
                    .map(|()| true)
            }
            // Only a look that keeps a final link reports one, i.e. only
            // under `-P`/`-d`: the link itself is reproduced by storing the
            // same target, verbatim, so a relative or dangling one survives.
            Kind::Link => {
                let stored = match origin {
                    Origin::Operand(path) => self
                        .fs
                        .parent(path)
                        .and_then(|(dir, name)| dir.readlink(&name)),
                    Origin::Held(dir, name) | Origin::Followed(dir, name) => dir.readlink(name),
                }
                .map_err(|errno| read_error(source, errno))?;
                let Some(replace) =
                    self.link_slot(source, spot, target, Onto::Refused, |_| probe.id)?
                else {
                    return Ok(false);
                };
                self.make_symlink(spot, &stored, replace)
                    .map_err(|errno| create_error(target, errno))?;
                self.report(source, target)?;
                Ok(true)
            }
            Kind::File => self.reproduce_file(origin, source, &probe, spot, target),
        }
    }

    /// Reproduce the held source directory `held` (known as `source`) at
    /// `spot` (known as `target`): merged into a directory already there, or
    /// made with its source's permission and sticky bits and its owner able to
    /// fill it, its entries copied through the shared walk, then given the
    /// bits GNU's `cp` gives it — its source's less the umask.
    fn reproduce_directory(
        &mut self,
        held: &F::Dir,
        source: &str,
        probe: &Probe,
        spot: Spot<'_, F::Dir>,
        target: &str,
    ) -> Result<(), CpError> {
        let stat = held.stat().map_err(|errno| read_error(source, errno))?;
        if stat.id != probe.id {
            return Err(read_error(source, Errno::Stale));
        }
        let existing = match spot {
            Spot::Operand(path) => self.fs.open_dir(path, true),
            Spot::Held(dir, name) => dir.open_dir(name),
        };
        let (into, settle, made_id) = match existing {
            Ok(existing) => {
                let id = existing
                    .stat()
                    .map_err(|errno| stat_error(target, errno))?
                    .id;
                (existing, None, id)
            }
            Err(Errno::NotADirectory) => {
                return Err(CpError::Overwrite {
                    source: String::from(source),
                    target: String::from(target),
                })
            }
            Err(Errno::NotFound) => {
                let (made, settle, id) = self.make_directory(spot, &stat, target)?;
                self.carry_attributes((held, source), &made, target)?;
                self.report(source, target)?;
                (made, Some(settle), id)
            }
            Err(errno) => return Err(stat_error(target, errno)),
        };
        self.descent
            .enter(stat.id, made_id)
            .map_err(|_| CpError::MemoryExhausted)?;
        let mut copy = TreeCopy {
            copier: &mut *self,
            root: &into,
            base: Level::new(None, target, stat.id, true, None),
            levels: Vec::new(),
        };
        let walked = walk(held, source, &mut copy);
        // A walk ended early leaves directories it made unsettled: each takes
        // its mode, deepest first, before the failure is answered, and one
        // that will not is lost to the failure already ending the walk.
        while let Some(level) = copy.levels.pop() {
            let _ = copy.close(level);
        }
        self.descent.leave();
        let settled = match settle {
            Some(settle) => settle
                .finish(&into)
                .map_err(|errno| write_error(target, errno)),
            None => Ok(()),
        };
        walked?;
        settled
    }

    /// Make the directory at `spot` for the source directory `stat`
    /// describes: asked for its permission and sticky bits (or the default
    /// mode under `--no-preserve=mode`) with its owner's read, write and
    /// search added, and given the latter outright when the umask withheld
    /// any, so its entries can be made. Answers it held, the mode it is left
    /// with once full, and its identity.
    fn make_directory(
        &self,
        spot: Spot<'_, F::Dir>,
        stat: &FileStat,
        target: &str,
    ) -> Result<(F::Dir, Settle, FileId), CpError> {
        let modes = DirModes::new(if self.options.source_modes {
            stat.mode
        } else {
            FS_NEW_DIR_MODE
        });
        let made = match spot {
            Spot::Operand(path) => self
                .fs
                .parent(path)
                .and_then(|(dir, name)| dir.make_dir(&name, modes.asked())),
            Spot::Held(dir, name) => dir.make_dir(name, modes.asked()),
        }
        .map_err(|errno| create_error(target, errno))?;
        let (born, settle) = modes
            .ready(&made)
            .map_err(|errno| write_error(target, errno))?;
        Ok((made, settle, born.id))
    }

    /// Put a non-directory source at `spot`: its bytes, a second name for its
    /// node (`-l`, or `--preserve=links` meeting the node again), or a
    /// symbolic link naming it (`-s`).
    fn reproduce_file(
        &mut self,
        origin: Origin<'_, F::Dir>,
        source: &str,
        probe: &Probe,
        spot: Spot<'_, F::Dir>,
        target: &str,
    ) -> Result<bool, CpError> {
        // A node already written under another of its names becomes a second
        // name there, whatever the contents mode: `--preserve=links` is about
        // keeping the *sources'* sharing, so it outranks copying the bytes.
        if let Some((first, made)) = self.linked.destination_of(probe) {
            // Reached through its own directory, and linked only while it is
            // still the node this run made there.
            let (first_dir, first_name) = self
                .fs
                .parent(first)
                .map_err(|errno| create_error(target, errno))?;
            let still = first_dir
                .open_node(&first_name)
                .and_then(|node| node.stat())
                .is_ok_and(|stat| stat.id == made);
            if !still {
                return Err(create_error(target, Errno::Stale));
            }
            let Some(replace) = self.link_slot(source, spot, target, Onto::Made, |_| made)? else {
                return Ok(false);
            };
            self.make_link(Origin::Held(&first_dir, &first_name), spot, replace)
                .map_err(|errno| create_error(target, errno))?;
            self.report(source, target)?;
            return Ok(true);
        }
        let written = match self.options.contents {
            Contents::Bytes => self.copy_file(origin, source, probe, spot, target)?,
            Contents::HardLink => {
                let Some(replace) =
                    self.link_slot(source, spot, target, Onto::Made, |_| probe.id)?
                else {
                    return Ok(false);
                };
                // Linking never follows a name, so a link this copy followed
                // is linked by the path of what it names, as GNU links it.
                let followed = match origin {
                    Origin::Operand(path) => match self.stat(path, FinalLink::Keep)? {
                        Some(own) if own.kind == Kind::Link => Some(
                            self.fs
                                .canonical(path)
                                .map_err(|errno| stat_error(path, errno))?,
                        ),
                        _ => None,
                    },
                    Origin::Held(..) | Origin::Followed(..) => None,
                };
                let existing = match &followed {
                    Some(resolved) => Origin::Operand(resolved.as_str()),
                    None => origin,
                };
                self.make_link(existing, spot, replace)
                    .map_err(|errno| create_error(target, errno))?;
                self.report(source, target)?;
                true
            }
            Contents::SymbolicLink => {
                let Some(replace) =
                    self.link_slot(source, spot, target, Onto::Refused, |_| probe.id)?
                else {
                    return Ok(false);
                };
                self.make_symlink(spot, source, replace)
                    .map_err(|errno| create_error(target, errno))?;
                self.report(source, target)?;
                true
            }
        };
        // A destination kept rather than written is no copy of this node.
        if written && self.options.preserve_links && probe.nlink > 1 {
            let made = self
                .occupant(spot, target, FinalLink::Keep)?
                .map_or(FileId::NONE, |made| made.id);
            self.linked.remember(probe, target, made);
        }
        Ok(written)
    }

    /// Whether a link may be put at `spot`, and whether in an occupant's
    /// place: [`None`] when the destination is kept, the occupant's place
    /// taken only under `-f` — without it the create fails, and the kernel says
    /// why. A link takes the occupant's place in one step where an operand
    /// names it, made before anything is replaced.
    ///
    /// A destination that already is the node the link would name (`names`,
    /// asked only of an occupied name) is the second name made, or the same
    /// file for a symbolic link, as GNU judges each.
    fn link_slot(
        &mut self,
        source: &str,
        spot: Spot<'_, F::Dir>,
        target: &str,
        onto: Onto,
        names: impl FnOnce(&Self) -> FileId,
    ) -> Result<Option<Slot>, CpError> {
        let Some(own) = self.occupant(spot, target, FinalLink::Keep)? else {
            return Ok(Some(Slot::Free));
        };
        let link_node = names(self);
        if !link_node.is_none() && own.id == link_node {
            return match onto {
                Onto::Made => Ok(None),
                Onto::Refused => Err(CpError::SameFile {
                    source: String::from(source),
                    target: String::from(target),
                }),
            };
        }
        if !self.consented(target)? {
            return Ok(None);
        }
        Ok(Some(if self.options.force {
            Slot::Taken(own.id)
        } else {
            Slot::Free
        }))
    }

    /// The existing-destination policy for a destination that is already
    /// there: `-n` skips it, `-i` asks, and the default overwrites. A declined
    /// question fails the run, as GNU's does; a `-n` skip does not.
    fn consented(&mut self, target: &str) -> Result<bool, CpError> {
        let agreed = match self.options.clobber {
            Clobber::Overwrite => return Ok(true),
            Clobber::Skip => return Ok(false),
            Clobber::Prompt => self
                .prompt
                .confirm(&format!("overwrite {}?", quote(target)))
                .map_err(CpError::Prompt)?,
        };
        if !agreed {
            self.carried.fail_quietly();
        }
        Ok(agreed)
    }

    /// Copy the regular file `origin`, the node `node` describes, to `spot` —
    /// its bytes with its holes, then its extended attributes — honouring the
    /// existing-destination policy and `-f` (remove a destination that cannot
    /// be written and create it afresh, once); whether it was written.
    fn copy_file(
        &mut self,
        origin: Origin<'_, F::Dir>,
        source: &str,
        node: &Probe,
        spot: Spot<'_, F::Dir>,
        target: &str,
    ) -> Result<bool, CpError> {
        let occupant = self.occupant(spot, target, FinalLink::Keep)?;
        // An operand's byte copy writes through a final link, so the node it
        // would empty is the one the link names.
        let emptied = match (occupant, spot) {
            (Some(link), Spot::Operand(path)) if link.kind == Kind::Link => {
                self.stat(path, FinalLink::Follow)?
            }
            (other, _) => other,
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
        let mut from = match origin {
            Origin::Operand(path) => self.fs.open_source(path),
            Origin::Held(dir, name) => dir.open_file(name, false),
            Origin::Followed(dir, name) => dir.follow(name, true),
        }
        .map_err(|errno| read_error(source, errno))?;
        let stat = from.stat().map_err(|errno| read_error(source, errno))?;
        if !node.id.is_none() && stat.id != node.id {
            return Err(read_error(source, Errno::Stale));
        }
        let mode = self.new_mode(&stat);
        let mut to = self.destination(
            spot,
            emptied.is_some(),
            occupant.map(|own| own.id),
            mode,
            target,
        )?;
        if self.buf.is_empty() && !tairix_util::fallible::grow_to(&mut self.buf, CHUNK_LEN, 0) {
            return Err(CpError::MemoryExhausted);
        }
        DataCopy::new(stat.size, self.options.sparse)
            .run(&mut from, &mut to, &mut self.buf)
            .map_err(|failure| match failure {
                DataError::Read(errno) => read_error(source, errno),
                DataError::Write(errno) => write_error(target, errno),
            })?;
        self.carry_attributes((&from, source), &to, target)?;
        self.report(source, target)?;
        Ok(true)
    }

    /// The file at `spot` a byte copy writes, empty: the one already there,
    /// keeping its own mode, when `exists`, else one made in `mode` less the
    /// umask; with `-f`, one that cannot be written or made is removed — only
    /// while the name holds `occupant`, the node looked at — and made afresh
    /// once.
    fn destination(
        &self,
        spot: Spot<'_, F::Dir>,
        exists: bool,
        occupant: Option<FileId>,
        mode: u32,
        target: &str,
    ) -> Result<<F::Dir as Dir>::File, CpError> {
        let opened = if exists {
            self.truncated(spot)
        } else {
            self.created(spot, mode)
        };
        match (opened, occupant) {
            (Ok(file), _) => Ok(file),
            // A link inside a destination tree is never written through.
            (Err(Errno::LinkLoop), Some(_)) if matches!(spot, Spot::Held(..)) => {
                Err(CpError::ThroughLink(String::from(target)))
            }
            (Err(_), Some(occupant)) if self.options.force => {
                // Only the node looked at goes; a removal error is
                // irrelevant if the create after it works.
                let expect = NodeExpect::of_node(occupant);
                let _ = match spot {
                    Spot::Operand(path) => self.fs.parent(path).and_then(|(dir, name)| {
                        dir.unlink(&name, UnlinkFlags::EXPECT_NODE, Some(&expect))
                    }),
                    Spot::Held(dir, name) => {
                        dir.unlink(name, UnlinkFlags::EXPECT_NODE, Some(&expect))
                    }
                };
                self.created(spot, mode)
                    .map_err(|errno| create_error(target, errno))
            }
            (Err(errno), _) => Err(create_error(target, errno)),
        }
    }

    fn truncated(&self, spot: Spot<'_, F::Dir>) -> Result<<F::Dir as Dir>::File, Errno> {
        match spot {
            Spot::Operand(path) => self.fs.truncate_existing(path),
            Spot::Held(dir, name) => {
                let mut file = dir.open_file(name, true)?;
                tairix_fscopy::Target::set_len(&mut file, 0)?;
                Ok(file)
            }
        }
    }

    fn created(&self, spot: Spot<'_, F::Dir>, mode: u32) -> Result<<F::Dir as Dir>::File, Errno> {
        match spot {
            Spot::Operand(path) => self
                .fs
                .parent(path)
                .and_then(|(dir, name)| dir.create_file(&name, mode)),
            Spot::Held(dir, name) => dir.create_file(name, mode),
        }
    }

    /// The symbolic link storing `stored` made at `spot`, `replace`-ing its
    /// occupant: in one step where an operand names it, and where a held
    /// directory does, only while the name still holds the occupant looked at.
    fn make_symlink(
        &self,
        spot: Spot<'_, F::Dir>,
        stored: &str,
        replace: Slot,
    ) -> Result<(), Errno> {
        match spot {
            Spot::Operand(path) => self.fs.symlink(stored, path, replace != Slot::Free),
            Spot::Held(dir, name) => {
                if let Slot::Taken(occupant) = replace {
                    unlink_node(dir, name, occupant)?;
                }
                dir.symlink(stored.as_bytes(), name)
            }
        }
    }

    /// A second name at `spot` for the node `existing` names, neither
    /// followed, `replace`-ing its occupant as [`Self::make_symlink`] does.
    fn make_link(
        &self,
        existing: Origin<'_, F::Dir>,
        spot: Spot<'_, F::Dir>,
        replace: Slot,
    ) -> Result<(), Errno> {
        let (from_parent, spot_parent);
        let (from_dir, from, follow) = match existing {
            Origin::Operand(from) => match spot {
                Spot::Operand(path) => return self.fs.link(from, path, replace != Slot::Free),
                Spot::Held(..) => {
                    from_parent = self.fs.parent(from)?;
                    (&from_parent.0, from_parent.1.as_str(), false)
                }
            },
            Origin::Held(from_dir, from) => (from_dir, from, false),
            Origin::Followed(from_dir, from) => (from_dir, from, true),
        };
        let (dir, name) = match spot {
            Spot::Held(dir, name) => (dir, name),
            Spot::Operand(path) => {
                spot_parent = self.fs.parent(path)?;
                (&spot_parent.0, spot_parent.1.as_str())
            }
        };
        if let Slot::Taken(occupant) = replace {
            unlink_node(dir, name, occupant)?;
        }
        from_dir.link(from, dir, name, follow)
    }

    /// Carry the extended attributes of `from`, known as `source`, to `at`,
    /// noting what its volume could not keep.
    fn carry_attributes<S: Node + ?Sized, T: Node + ?Sized>(
        &mut self,
        (from, source): (&S, &str),
        at: &T,
        target: &str,
    ) -> Result<(), CpError> {
        let left = copy_attributes(from, at, self.options.attributes).map_err(|err| match err {
            AttrError::Read(errno) => read_error(source, errno),
            AttrError::Write { key, error } => CpError::Attribute {
                key,
                target: String::from(target),
                error,
            },
        })?;
        self.omissions.note(&left);
        Ok(())
    }

    /// The mode a file the copy makes for the source `stat` describes is born
    /// with, less the umask: the source's permission bits, or, under
    /// `--no-preserve=mode`, the mode a new file takes when nothing names
    /// one. GNU's `cp` drops the set-ID and sticky bits it does not preserve.
    fn new_mode(&self, stat: &FileStat) -> u32 {
        if self.options.source_modes {
            file_mode(stat.mode)
        } else {
            FS_NEW_FILE_MODE
        }
    }

    /// What `spot` holds, a final link followed under `follow` where an
    /// operand names it; a name in a held directory is never followed.
    fn occupant(
        &self,
        spot: Spot<'_, F::Dir>,
        target: &str,
        follow: FinalLink,
    ) -> Result<Option<Probe>, CpError> {
        match spot {
            Spot::Operand(path) => self.stat(path, follow),
            Spot::Held(dir, name) => {
                absent_as_none(target, dir.open_node(name).and_then(|node| node.stat()))
            }
        }
    }

    /// What `path` names under `follow`.
    fn probe(&self, path: &str, follow: FinalLink) -> Result<Probe, CpError> {
        self.fs
            .open_node(path, follow == FinalLink::Follow)
            .and_then(|node| node.stat())
            .map(|stat| Probe::of(&stat))
            .map_err(|errno| stat_error(path, errno))
    }

    /// Inspect `path`, mapping a missing path to [`None`] so a destination
    /// can be probed for existence without treating absence as a failure.
    fn stat(&self, path: &str, follow: FinalLink) -> Result<Option<Probe>, CpError> {
        absent_as_none(
            path,
            self.fs
                .open_node(path, follow == FinalLink::Follow)
                .and_then(|node| node.stat()),
        )
    }

    /// The follow posture an operand's own look uses: GNU's, where a
    /// recursive copy reproduces an operand's link unless `-H`/`-L` (or `-l`,
    /// which links what the link names) says otherwise.
    fn operand_follow(&self) -> FinalLink {
        let follows = match self.options.dereference {
            Dereference::Never => false,
            Dereference::Operands | Dereference::Always => true,
            Dereference::Default => {
                !self.options.recursive || self.options.contents == Contents::HardLink
            }
        };
        if follows {
            FinalLink::Follow
        } else {
            FinalLink::Keep
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

    /// Copy the entry `entry` of the held source directory `dir`, known as
    /// `path`, into the held destination directory `into`, known as
    /// `target`. A directory is answered with the level its walk descends
    /// into; a link the run follows is copied as what it names.
    fn copy_entry(
        &mut self,
        (dir, entry, path): (&F::Dir, &Entry, &str),
        into: &F::Dir,
        target: String,
    ) -> Result<Option<Level<F::Dir>>, CpError> {
        if entry.kind == Kind::Link && self.follows_children() {
            // Followed as the user asked, from the directory holding it: what
            // it names is copied as what it is.
            let followed = dir
                .follow(&entry.name, false)
                .and_then(|node| node.stat())
                .map(|stat| Probe::of(&stat))
                .map_err(|errno| stat_error(path, errno))?;
            if followed.kind == Kind::Directory && self.descent.refuses(followed.id) {
                return Err(CpError::IntoItself {
                    source: String::from(path),
                    target,
                });
            }
            self.copy_known(
                Origin::Followed(dir, &entry.name),
                path,
                followed,
                Spot::Held(into, &entry.name),
                &target,
            )?;
            return Ok(None);
        }
        if entry.kind != Kind::Directory {
            // What the listing said is what the open must find.
            self.copy_known(
                Origin::Held(dir, &entry.name),
                path,
                Probe::listed(entry),
                Spot::Held(into, &entry.name),
                &target,
            )?;
            return Ok(None);
        }
        // A mount point is listed as the directory beneath it.
        let (held, node) = dir
            .open_node(&entry.name)
            .and_then(|held| held.stat().map(|node| (held, node)))
            .map_err(|errno| stat_error(path, errno))?;
        let probe = Probe::of(&node);
        if probe.kind != Kind::Directory {
            return Err(stat_error(path, Errno::Stale));
        }
        if self.descent.refuses(probe.id) {
            return Err(CpError::IntoItself {
                source: String::from(path),
                target,
            });
        }
        let (made, settle, made_id) = match into.open_dir(&entry.name) {
            Ok(existing) => {
                let id = existing
                    .stat()
                    .map_err(|errno| stat_error(&target, errno))?
                    .id;
                (existing, None, id)
            }
            Err(Errno::NotADirectory) => {
                return Err(CpError::Overwrite {
                    source: String::from(path),
                    target,
                })
            }
            Err(Errno::NotFound) => {
                let (made, settle, id) =
                    self.make_directory(Spot::Held(into, &entry.name), &node, &target)?;
                self.carry_attributes((&held, path), &made, &target)?;
                self.report(path, &target)?;
                (made, Some(settle), id)
            }
            Err(errno) => return Err(stat_error(&target, errno)),
        };
        self.descent
            .enter(probe.id, made_id)
            .map_err(|_| CpError::MemoryExhausted)?;
        Ok(Some(Level::new(
            Some(made),
            &target,
            probe.id,
            false,
            settle,
        )))
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

/// One directory a tree copy is in: the destination directory its entries
/// go in, the source directory it copies, and the mode a directory the copy
/// made is left with.
struct Level<D> {
    /// `None` for the operand's own destination, which the copy borrows.
    made: Option<D>,
    target: String,
    source: FileId,
    /// Whether the source directory the walk holds here was checked to be
    /// `source`, and found another.
    checked: bool,
    swapped: bool,
    settle: Option<Settle>,
}

impl<D> Level<D> {
    fn new(
        made: Option<D>,
        target: &str,
        source: FileId,
        checked: bool,
        settle: Option<Settle>,
    ) -> Self {
        Self {
            made,
            target: String::from(target),
            source,
            checked,
            swapped: false,
            settle,
        }
    }
}

/// The walk of a source directory that copies it, entry by entry, into the
/// destination directories it holds; an entry that fails is reported and the
/// rest still copied, as GNU's walk does.
struct TreeCopy<'t, 'a, F: FileSystem> {
    copier: &'t mut Copier<'a, F>,
    root: &'t F::Dir,
    base: Level<F::Dir>,
    levels: Vec<Level<F::Dir>>,
}

impl<F: FileSystem> TreeCopy<'_, '_, F> {
    fn top_mut(&mut self) -> &mut Level<F::Dir> {
        match self.levels.last_mut() {
            Some(level) => level,
            None => &mut self.base,
        }
    }

    /// Whether the entries of the source directory the walk holds here may be
    /// copied: only when it is the directory entered, which is checked the
    /// first time the walk shows it, and said once when it is not — one
    /// swapped in between the look and the walk's own open is not copied.
    fn admits(&mut self, dir: &F::Dir, path: &str) -> Result<bool, CpError> {
        let level = self.top_mut();
        if level.checked {
            return Ok(!level.swapped);
        }
        level.checked = true;
        let source = level.source;
        let errno = match dir.stat() {
            Ok(stat) if stat.id == source => return Ok(true),
            Ok(_) => Errno::Stale,
            Err(errno) => errno,
        };
        self.top_mut().swapped = true;
        self.copier
            .carried
            .carry(read_error(path, errno))
            .map(|()| false)
    }

    /// Leave a level the walk is done with: the directory the copy made for
    /// it given its mode.
    fn close(&mut self, level: Level<F::Dir>) -> Result<(), CpError> {
        self.copier.descent.leave();
        match (level.made, level.settle) {
            (Some(made), Some(settle)) => settle
                .finish(&made)
                .map_err(|errno| write_error(&level.target, errno)),
            _ => Ok(()),
        }
    }
}

impl<F: FileSystem> Visitor<F::Dir> for TreeCopy<'_, '_, F> {
    type Error = CpError;

    fn enter(&mut self, dir: &F::Dir, entry: &Entry, path: &str) -> Result<Step<F::Dir>, CpError> {
        if !self.admits(dir, dir_name(path))? {
            return Ok(Step::Skip);
        }
        // Each field borrowed apart, so the copier may be lent on.
        let level = self.levels.last().unwrap_or(&self.base);
        let into = level.made.as_ref().unwrap_or(self.root);
        let target = join(&level.target, &entry.name);
        match self.copier.copy_entry((dir, entry, path), into, target) {
            Ok(None) => Ok(Step::Skip),
            Ok(Some(level)) => {
                if self.levels.try_reserve(1).is_err() {
                    let _ = self.close(level);
                    return Err(CpError::MemoryExhausted);
                }
                self.levels.push(level);
                Ok(Step::Descend)
            }
            Err(failure) => self.copier.carried.carry(failure).map(|()| Step::Skip),
        }
    }

    fn leave(&mut self, _: &F::Dir, _: &Entry, path: &str, held: &F::Dir) -> Result<(), CpError> {
        let Some(level) = self.levels.pop() else {
            return Ok(());
        };
        // An empty directory is first shown here, so a swap is found here.
        let swapped = !level.checked && held.stat().is_ok_and(|stat| stat.id != level.source);
        let closed = self.close(level);
        self.copier.carried.absorb(closed)?;
        if swapped {
            return self.copier.carried.carry(read_error(path, Errno::Stale));
        }
        Ok(())
    }

    fn refused(&mut self, _: &F::Dir, _: &Entry, path: &str, errno: Errno) -> Result<(), CpError> {
        if let Some(level) = self.levels.pop() {
            let closed = self.close(level);
            self.copier.carried.absorb(closed)?;
        }
        self.copier.carried.carry(read_error(path, errno))
    }

    fn unlisted_root(&mut self, path: &str, errno: Errno) -> Result<(), CpError> {
        Err(read_error(path, errno))
    }
}

fn absent_as_none(path: &str, probe: Result<FileStat, Errno>) -> Result<Option<Probe>, CpError> {
    match probe {
        Ok(stat) => Ok(Some(Probe::of(&stat))),
        Err(Errno::NotFound) => Ok(None),
        Err(errno) => Err(stat_error(path, errno)),
    }
}

/// The destination `path` refused as `cp`'s target, `directory` when `-t`
/// named it.
fn target_error(path: &str, directory: bool, errno: Errno) -> CpError {
    CpError::Target {
        path: String::from(path),
        directory,
        errno,
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
        create_error, place, read_error, run as engine_run, stat_error, target_error, write_error,
        Seams, USAGE,
    };
    use crate::command::{Clobber, Command, Contents, Dereference, Options, TargetMode};
    use crate::error::CpError;
    use crate::io::{Dir, FileSystem, Kind, Node, Output, Prompt, Roots};
    use alloc::boxed::Box;
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;
    use core::cell::{Cell, RefCell};
    use tairix_abi::{Errno, FileId, RenameFlags};
    use tairix_fscopy::fixture::{Handle, Tree};
    use tairix_fscopy::Target;
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

    /// The engine, answering what it reported when it did not run clean: a
    /// failed operand is reported and the next still copied. What it wrote
    /// on its diagnostics stream is appended to `out`, as one terminal
    /// shows both.
    fn run(command: Command, fs: &MemFs, out: &Recorder) -> Result<(), String> {
        let diagnostics = Recorder::new();
        let outcome = engine_run(
            command,
            None,
            &seams(fs, &NeverAsked, &NoHelp, out, &diagnostics),
        );
        out.bytes
            .borrow_mut()
            .extend_from_slice(&diagnostics.bytes.borrow());
        match outcome {
            Ok(true) => Ok(()),
            Ok(false) => Err(diagnostics.text()),
            Err(fatal) => Err(format!("cp: {fatal}\n")),
        }
    }

    /// `failure` as the tool reports it.
    fn shown(failure: &CpError) -> Result<(), String> {
        Err(format!("cp: {failure}\n"))
    }

    fn seams<'a>(
        fs: &'a MemFs,
        prompt: &'a dyn Prompt,
        help: &'a dyn HelpSource,
        out: &'a dyn Output,
        diagnostics: &'a dyn Output,
    ) -> Seams<'a, MemFs> {
        Seams {
            fs,
            prompt,
            help,
            out,
            diagnostics,
            keys: BuildSipHash13::UNKEYED,
        }
    }

    /// The shared in-memory tree, with what `cp` reaches beyond its seam and
    /// the records the tests read back.
    struct MemFs {
        tree: Tree,
        /// Destinations a byte copy may not open for writing, and why.
        unwritable: Vec<(String, Errno)>,
        /// The names a link took the place of, in order.
        replaced: RefCell<Vec<String>>,
    }

    impl MemFs {
        fn new() -> Self {
            Self::of(Tree::new())
        }

        fn of(tree: Tree) -> Self {
            Self {
                tree,
                unwritable: Vec::new(),
                replaced: RefCell::new(Vec::new()),
            }
        }

        fn with(self, build: impl FnOnce(Tree) -> Tree) -> Self {
            Self {
                tree: build(self.tree),
                ..self
            }
        }

        fn file(self, path: &str, contents: &[u8]) -> Self {
            self.with(|tree| tree.file(path, contents))
        }

        fn dir(self, path: &str) -> Self {
            self.with(|tree| tree.dir(path))
        }

        /// A symbolic link at `path` storing `target` verbatim.
        fn symlink_at(self, path: &str, target: &str) -> Self {
            self.with(|tree| tree.link(path, target))
        }

        /// A second name at `path` for the node `object` already names.
        fn second_name(self, path: &str, object: &str) -> Self {
            self.with(|tree| tree.hard_link(path, object))
        }

        /// A sparse file at `path`, `len` bytes long, storing `runs` of
        /// `(offset, bytes)`; its holes read as zeros.
        fn sparse(self, path: &str, len: usize, runs: &[(u64, &[u8])]) -> Self {
            self.with(|tree| tree.sparse(path, len, runs))
        }

        /// An extended attribute on `path`.
        fn attr(self, path: &str, key: &str, value: &[u8]) -> Self {
            self.with(|tree| tree.attr(path, key, value))
        }

        /// Setting `key` anywhere is refused with `errno`.
        fn refuses_attr(self, key: &str, errno: Errno) -> Self {
            self.with(|tree| tree.refuses_attr(key, errno))
        }

        /// `path` holding `mode`.
        fn with_mode(self, path: &str, mode: u32) -> Self {
            self.with(|tree| tree.with_mode(path, mode))
        }

        /// A destination at `path` refused with `errno` until it is removed:
        /// one already there may not be written, a new one not made.
        fn create_fails(mut self, path: &str, errno: Errno) -> Self {
            if self.tree.exists(path) {
                self.unwritable.push((String::from(path), errno));
                self
            } else {
                self.with(|tree| tree.refuse(&format!("create_file {path}"), errno))
            }
        }

        /// Reading the file `path`, or listing the directory, answers `errno`.
        fn read_fails(self, path: &str, errno: Errno) -> Self {
            self.with(|tree| {
                tree.refuse(&format!("read {path}"), errno)
                    .refuse(&format!("entries {path}"), errno)
            })
        }

        fn write_fails(self, path: &str, errno: Errno) -> Self {
            self.with(|tree| tree.refuse(&format!("write {path}"), errno))
        }

        fn contents(&self, path: &str) -> Option<Vec<u8>> {
            self.tree.bytes(path)
        }

        fn has_dir(&self, path: &str) -> bool {
            self.tree.kind(path) == Some(Kind::Directory)
        }

        fn target_of(&self, path: &str) -> Option<String> {
            self.tree.link_target(path)
        }

        fn mode_of(&self, path: &str) -> u32 {
            self.tree.mode(path).unwrap_or(0)
        }

        fn attr_of(&self, path: &str, key: &str) -> Option<Vec<u8>> {
            self.tree.attr_of(path, key)
        }

        fn written_to(&self, path: &str) -> usize {
            self.tree.written(path)
        }

        /// The names removed, in order.
        fn removed(&self) -> Vec<String> {
            self.tree
                .calls()
                .into_iter()
                .filter_map(|call| call.strip_prefix("unlink ").map(String::from))
                .collect()
        }

        fn replaced(&self) -> Vec<String> {
            self.replaced.borrow().clone()
        }

        /// How many names the node at `path` has.
        fn names_of(&self, path: &str) -> u32 {
            self.tree
                .open_node(path, false)
                .and_then(|node| node.stat())
                .map_or(0, |stat| stat.nlink)
        }

        /// Whether `path` is another name for the node `object` names.
        fn is_second_leaf_name(&self, path: &str, object: &str) -> bool {
            path != object
                && self.tree.id(path).is_some()
                && self.tree.id(path) == self.tree.id(object)
        }

        /// Put a link at `name` in `dir` in one step: made aside, renamed over.
        fn replace(
            &self,
            dir: &Handle,
            name: &str,
            spelled: &str,
            make: impl FnOnce(&str) -> Result<(), Errno>,
        ) -> Result<(), Errno> {
            let aside = format!("{name}.replacing");
            make(&aside)?;
            dir.rename(&aside, dir, name, RenameFlags::empty(), None)?;
            self.replaced.borrow_mut().push(String::from(spelled));
            Ok(())
        }
    }

    impl Roots for MemFs {
        type Dir = Handle;

        fn open_dir(&self, path: &str, follow: bool) -> Result<Handle, Errno> {
            self.tree.open_dir(path, follow)
        }

        fn parent(&self, path: &str) -> Result<(Handle, String), Errno> {
            self.tree.parent(path)
        }

        fn open_node(&self, path: &str, follow: bool) -> Result<Handle, Errno> {
            self.tree.open_node(path, follow)
        }
    }

    impl FileSystem for MemFs {
        fn open_source(&self, path: &str) -> Result<Handle, Errno> {
            self.tree.open_file(path, true, false)
        }

        fn truncate_existing(&self, path: &str) -> Result<Handle, Errno> {
            if let Some(&(_, errno)) = self.unwritable.iter().find(|(held, _)| held == path) {
                return Err(errno);
            }
            let mut file = self.tree.open_file(path, true, true)?;
            file.set_len(0)?;
            Ok(file)
        }

        fn canonical(&self, path: &str) -> Result<String, Errno> {
            self.tree.canonical(path)
        }

        fn symlink(&self, target: &str, link: &str, replace: bool) -> Result<(), Errno> {
            let (dir, name) = self.tree.parent(link)?;
            if replace && dir.open_node(&name).is_ok() {
                return self.replace(&dir, &name, link, |aside| {
                    dir.symlink(target.as_bytes(), aside)
                });
            }
            dir.symlink(target.as_bytes(), &name)
        }

        fn link(&self, existing: &str, new: &str, replace: bool) -> Result<(), Errno> {
            let (from, from_name) = self.tree.parent(existing)?;
            let (dir, name) = self.tree.parent(new)?;
            if replace && dir.open_node(&name).is_ok() {
                return self.replace(&dir, &name, new, |aside| {
                    from.link(&from_name, &dir, aside, false)
                });
            }
            from.link(&from_name, &dir, &name, false)
        }
    }

    /// Captures every byte written and every advisory record; optionally
    /// fails on the first write.
    struct Recorder {
        bytes: RefCell<Vec<u8>>,
        records: RefCell<Vec<u8>>,
        /// The writes it takes before it fails every one.
        writes_left: Cell<usize>,
    }

    impl Recorder {
        fn new() -> Self {
            Self {
                bytes: RefCell::new(Vec::new()),
                records: RefCell::new(Vec::new()),
                writes_left: Cell::new(usize::MAX),
            }
        }

        fn failing() -> Self {
            Self::failing_after(0)
        }

        fn failing_after(writes: usize) -> Self {
            Self {
                writes_left: Cell::new(writes),
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
            let Some(left) = self.writes_left.get().checked_sub(1) else {
                return Err(Errno::NotFound);
            };
            self.writes_left.set(left);
            self.bytes.borrow_mut().extend_from_slice(bytes);
            Ok(())
        }

        fn info(&self, record: &[u8]) {
            self.records.borrow_mut().extend_from_slice(record);
        }
    }

    /// Run `command` with its diagnostics apart from its output.
    fn run_apart(command: Command, fs: &MemFs) -> (Result<(), String>, Recorder, Recorder) {
        let (out, diagnostics) = (Recorder::new(), Recorder::new());
        let result = match engine_run(
            command,
            None,
            &seams(fs, &NeverAsked, &NoHelp, &out, &diagnostics),
        ) {
            Ok(true) => Ok(()),
            Ok(false) => Err(diagnostics.text()),
            Err(fatal) => Err(format!("cp: {fatal}\n")),
        };
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
    fn a_directory_made_inside_a_tree_carries_its_attributes() {
        let fs = MemFs::new()
            .dir("/src/inner")
            .attr("/src/inner", "user.riscos.filetype", b"fff")
            .file("/src/inner/f", b"f")
            .dir("/dst");
        assert_eq!(
            run(copy(true, false, &["/src"], "/dst"), &fs, &Recorder::new()),
            Ok(())
        );
        assert_eq!(
            fs.tree.attr_of("/dst/src/inner", "user.riscos.filetype"),
            Some(b"fff".to_vec())
        );
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
            shown(&CpError::Attribute {
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
        fs.link("/a", "/second", false).expect("a second name");
        for dest in ["/a", "/second", "/alias"] {
            let (result, _, _) = run_apart(copy(false, true, &["/a"], dest), &fs);
            assert_eq!(
                result,
                shown(&CpError::SameFile {
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
                "setting attribute 'user.'$'\\033''[2J' for '/fat/a': {}",
                Errno::NotSupported
            )
        );
    }

    #[test]
    fn an_attribute_key_is_quoted_as_every_name_is() {
        let refused = CpError::Attribute {
            key: String::from("user.it's\u{202e}gpj"),
            target: String::from("/fat/a"),
            error: Errno::NotSupported,
        };
        assert_eq!(
            refused.to_string(),
            alloc::format!(
                "setting attribute 'user.it'\\''s'$'\\342\\200\\256''gpj' for '/fat/a': {}",
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

    /// GNU 9.4 under umask `022`: a new copy is born with its source's
    /// permission bits less the umask, its set-ID and sticky bits dropped.
    #[test]
    fn a_new_copy_takes_its_sources_permission_bits_under_the_umask() {
        for (source, copied) in [
            (0o600, 0o600),
            (0o755, 0o755),
            (0o777, 0o755),
            (0o4755, 0o755),
            (0o1777, 0o755),
            (0o444, 0o444),
        ] {
            let fs = MemFs::new().file("/a", b"x").with_mode("/a", source);
            let (result, _, _) = run_apart(copy(false, false, &["/a"], "/b"), &fs);
            assert_eq!(result, Ok(()), "{source:o}");
            assert_eq!(fs.mode_of("/b"), copied, "{source:o}");
        }
    }

    /// GNU 9.4: copying a `0600` file onto an existing `0644` one leaves the
    /// destination `0644`.
    #[test]
    fn an_existing_destination_keeps_its_mode() {
        let fs = MemFs::new()
            .file("/a", b"secret")
            .with_mode("/a", 0o600)
            .file("/b", b"old")
            .with_mode("/b", 0o644);
        let (result, _, _) = run_apart(copy(false, false, &["/a"], "/b"), &fs);
        assert_eq!(result, Ok(()));
        assert_eq!(fs.contents("/b").as_deref(), Some(&b"secret"[..]));
        assert_eq!(fs.mode_of("/b"), 0o644);
    }

    /// GNU 9.4: `--no-preserve=mode` makes a new copy `0666` less the
    /// umask, and a new directory `0777` less it.
    #[test]
    fn no_preserve_mode_makes_each_copy_in_the_default_mode() {
        let fs = MemFs::new()
            .dir("/d")
            .with_mode("/d", 0o700)
            .file("/d/f", b"x")
            .with_mode("/d/f", 0o600);
        let plain = Command::Copy {
            options: Options {
                recursive: true,
                source_modes: false,
                ..Options::DEFAULT
            },
            sources: alloc::vec![String::from("/d")],
            dest: String::from("/e"),
        };
        let (result, _, _) = run_apart(plain, &fs);
        assert_eq!(result, Ok(()));
        assert_eq!(fs.mode_of("/e"), 0o755);
        assert_eq!(fs.mode_of("/e/f"), 0o644);
    }

    /// D1485, GNU 9.4's result: a directory copied ends with its source's
    /// permission and sticky bits less the umask — a `0500` one `0500`, not
    /// widened by the owner's write and search it held while it was filled.
    #[test]
    fn a_copied_directory_ends_with_its_sources_bits_less_the_umask() {
        for (source, made) in [
            (0o700, 0o700),
            (0o750, 0o750),
            (0o500, 0o500),
            (0o1777, 0o1755),
        ] {
            let fs = MemFs::new()
                .dir("/d")
                .with_mode("/d", source)
                .file("/d/f", b"x")
                .with_mode("/d/f", 0o600);
            let (result, _, _) = run_apart(copy(true, false, &["/d"], "/e"), &fs);
            assert_eq!(result, Ok(()), "{source:o}");
            assert_eq!(fs.mode_of("/e"), made, "{source:o}");
            assert_eq!(fs.mode_of("/e/f"), 0o600, "{source:o}");
        }
    }

    /// D1485: a umask withholding the owner's write and search no longer stops
    /// a directory being filled — each is opened to its owner while its
    /// entries are made — and each still ends with its source's bits less the
    /// umask.
    #[test]
    fn a_umask_withholding_the_owners_write_does_not_stop_the_fill() {
        let fs = MemFs::of(
            Tree::new()
                .dir("/d/sub")
                .file("/d/f", b"x")
                .file("/d/sub/g", b"y")
                .with_mode("/d", 0o755)
                .with_umask(0o277)
                .checking_permissions(),
        );
        let (result, _, _) = run_apart(copy(true, false, &["/d"], "/e"), &fs);
        assert_eq!(result, Ok(()));
        assert_eq!(fs.contents("/e/f").as_deref(), Some(&b"x"[..]));
        assert_eq!(fs.contents("/e/sub/g").as_deref(), Some(&b"y"[..]));
        assert_eq!(fs.mode_of("/e"), 0o500);
        assert_eq!(fs.mode_of("/e/sub"), 0o500);
        assert_eq!(fs.mode_of("/e/f"), 0o400);
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
        assert!(fs.is_second_leaf_name("/b.txt", "/a.txt"));
        assert_eq!(fs.names_of("/a.txt"), 2, "no second copy of the bytes");
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
            shown(&create_error("/b.txt", Errno::AlreadyExists))
        );
        assert_eq!(fs.contents("/b.txt"), Some(b"old".to_vec()));
    }

    #[test]
    fn force_puts_the_second_name_in_the_taken_names_place() {
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
        assert!(fs.removed().is_empty(), "nothing is removed first");
        assert_eq!(fs.replaced(), ["/b.txt"]);
        assert!(fs.is_second_leaf_name("/b.txt", "/a.txt"));
    }

    /// GNU 9.4 takes a second name the destination already is as made, saying
    /// nothing even under `-v`, where removing it first would lose the file.
    #[test]
    fn a_second_name_the_destination_already_is_is_left_as_it_is() {
        for dest in ["/a.txt", "/h"] {
            let fs = MemFs::new()
                .file("/a.txt", b"hello")
                .second_name("/h", "/a.txt");
            let out = Recorder::new();
            let options = Options {
                contents: Contents::HardLink,
                force: true,
                verbose: true,
                ..Options::DEFAULT
            };
            assert_eq!(
                run(copy_with(options, &["/a.txt"], dest), &fs, &out),
                Ok(()),
                "{dest}"
            );
            assert!(fs.replaced().is_empty() && fs.removed().is_empty());
            assert_eq!(fs.contents("/a.txt"), Some(b"hello".to_vec()));
            assert_eq!(out.text(), "");
        }
    }

    /// GNU 9.4 refuses a symbolic link put on the node it would name, under
    /// any of its names, and a link copied onto itself.
    #[test]
    fn a_symbolic_link_onto_the_file_it_names_is_the_same_file() {
        let fs = MemFs::new()
            .file("/a.txt", b"hello")
            .second_name("/h", "/a.txt")
            .symlink_at("/sl", "/a.txt");
        let out = Recorder::new();
        for dest in ["/a.txt", "/h"] {
            let options = Options {
                contents: Contents::SymbolicLink,
                force: true,
                ..Options::DEFAULT
            };
            assert_eq!(
                run(copy_with(options, &["/a.txt"], dest), &fs, &out),
                shown(&CpError::SameFile {
                    source: String::from("/a.txt"),
                    target: String::from(dest),
                }),
                "{dest}"
            );
        }
        let options = Options {
            dereference: Dereference::Never,
            force: true,
            ..Options::DEFAULT
        };
        assert_eq!(
            run(copy_with(options, &["/sl"], "/sl"), &fs, &out),
            shown(&CpError::SameFile {
                source: String::from("/sl"),
                target: String::from("/sl"),
            })
        );
        assert!(fs.replaced().is_empty() && fs.removed().is_empty());
        assert_eq!(fs.target_of("/sl").as_deref(), Some("/a.txt"));
        assert_eq!(fs.contents("/a.txt"), Some(b"hello".to_vec()));
    }

    /// A link takes an occupied name's place in one step under `-f`.
    #[test]
    fn force_puts_a_symbolic_link_in_the_taken_names_place() {
        let fs = MemFs::new().file("/a.txt", b"hello").file("/b.txt", b"old");
        let out = Recorder::new();
        let options = Options {
            contents: Contents::SymbolicLink,
            force: true,
            ..Options::DEFAULT
        };
        assert_eq!(
            run(copy_with(options, &["/a.txt"], "/b.txt"), &fs, &out),
            Ok(())
        );
        assert!(fs.removed().is_empty());
        assert_eq!(fs.replaced(), ["/b.txt"]);
        assert_eq!(fs.target_of("/b.txt").as_deref(), Some("/a.txt"));
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
            shown(&stat_error("/alias", Errno::NotFound))
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
        assert!(fs.is_second_leaf_name("/dst/alias", "/src/real"));
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
        assert!(fs.is_second_leaf_name("/x", "/src/real"));
    }

    // --- a directory copied into itself -----------------------------------

    #[test]
    fn a_directory_copy_into_its_own_subtree_is_refused_before_anything_is_written() {
        let fs = MemFs::new().dir("/a").dir("/a/x").file("/a/f", b"bytes");
        let out = Recorder::new();
        assert_eq!(
            run(copy(true, false, &["/a"], "/a/b"), &fs, &out),
            shown(&CpError::IntoItself {
                source: String::from("/a"),
                target: String::from("/a/b"),
            })
        );
        assert!(!fs.has_dir("/a/b"), "nothing was made");
        // Into an existing directory of its own, the copy would land at
        // `/a/x/a`, which is inside it too.
        assert_eq!(
            run(copy(true, false, &["/a"], "/a/x"), &fs, &out),
            shown(&CpError::IntoItself {
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
            shown(&CpError::IntoItself {
                source: String::from("/a"),
                target: String::from("/b/to-a/c"),
            })
        );
        assert!(!fs.has_dir("/a/c"));
    }

    #[test]
    fn a_source_whose_path_no_longer_resolves_is_named_as_the_source() {
        // `/loop` names itself: the source's node is found, its path resolves
        // to nothing.
        let fs = MemFs::new()
            .dir("/loop/src")
            .file("/loop/src/f", b"bytes")
            .dir("/b")
            .symlink_at("/loop", "/loop");
        assert_eq!(
            run(
                copy(true, false, &["/loop/src"], "/b/c"),
                &fs,
                &Recorder::new()
            ),
            shown(&CpError::Stat {
                path: String::from("/loop/src"),
                errno: Errno::LinkLoop,
            })
        );
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
            shown(&CpError::SameFile {
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
            shown(&CpError::IntoItself {
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
        assert!(fs.is_second_leaf_name("/dst/two", "/dst/one"));
        assert_eq!(
            fs.names_of("/dst/two"),
            2,
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
                &seams(&fs, &prompt, &NoHelp, &out, &out),
            ),
            Ok(false),
            "the declined question fails the run"
        );
        assert_eq!(fs.contents("/dst/one"), Some(b"unrelated".to_vec()));
        assert_eq!(fs.contents("/dst/two"), Some(b"shared".to_vec()));
        assert!(!fs.is_second_leaf_name("/dst/two", "/dst/one"));
    }

    /// A source named again, however its directory is spelt, is skipped with
    /// GNU 9.4's warning and the run goes on.
    #[test]
    fn a_source_named_twice_is_warned_about_and_copied_once() {
        let fs = MemFs::new()
            .dir("/d")
            .dir("/s")
            .file("/s/f", b"one")
            .file("/s/g", b"two");
        let out = Recorder::new();
        assert_eq!(
            run(
                copy(false, false, &["/s/f", "/s/g", "/s/f"], "/d"),
                &fs,
                &out
            ),
            Ok(())
        );
        assert_eq!(
            out.text(),
            "cp: warning: source file '/s/f' specified more than once\n"
        );
        assert_eq!(fs.contents("/d/f"), Some(b"one".to_vec()));
        assert_eq!(fs.contents("/d/g"), Some(b"two".to_vec()));

        let fs = MemFs::new()
            .dir("/d")
            .dir("/s")
            .dir("/s/sub")
            .file("/s/sub/z", b"z");
        let out = Recorder::new();
        assert_eq!(
            run(copy(true, false, &["/s/sub", "/s/sub/"], "/d"), &fs, &out),
            Ok(())
        );
        assert_eq!(
            out.text(),
            "cp: warning: source directory '/s/sub/' specified more than once\n"
        );
        assert_eq!(fs.contents("/d/sub/z"), Some(b"z".to_vec()));
    }

    /// An entry is its directory as the kernel resolves it and its own name
    /// unfollowed, so only another spelling of the same entry matches.
    #[test]
    fn an_entry_is_its_resolved_directory_and_its_unfollowed_name() {
        let fs = MemFs::new()
            .dir("/s")
            .dir("/s/sub")
            .file("/s/f", b"one")
            .symlink_at("/up", "/s")
            .symlink_at("/s/link", "/s/f");
        let node = FileId {
            volume: [9; 16],
            node: 1,
        };
        let at = |path| place(&fs, node, path).expect("the fixture resolves it");
        for spelling in ["/s/./f", "/up/f", "/up/../s/f", "/s//f"] {
            assert_eq!(at(spelling), at("/s/f"), "{spelling}");
        }
        assert_eq!(at("/s/sub/"), at("/s/sub"));
        assert_ne!(at("/s/link"), at("/s/f"));
        assert_ne!(at("/up"), at("/s"));
        assert_eq!(at("/"), at("///"));
    }

    /// Another entry for a source's node is a source of its own, as GNU 9.4
    /// copies it: a link and what it leads to, or a second hard link.
    #[test]
    fn another_entry_for_a_source_s_node_is_copied_in_its_own_right() {
        let fs = MemFs::new()
            .dir("/d")
            .dir("/s")
            .file("/s/f", b"one")
            .symlink_at("/s/link", "/s/f")
            .second_name("/s/hard", "/s/f");
        let out = Recorder::new();
        assert_eq!(
            run(
                copy(false, false, &["/s/f", "/s/link", "/s/hard"], "/d"),
                &fs,
                &out
            ),
            Ok(())
        );
        assert_eq!(out.text(), "");
        for copied in ["/d/f", "/d/link", "/d/hard"] {
            assert_eq!(fs.contents(copied), Some(b"one".to_vec()), "{copied}");
        }

        let fs = MemFs::new()
            .dir("/d")
            .dir("/a")
            .dir("/b")
            .file("/a/f", b"one")
            .second_name("/b/f", "/a/f");
        assert_eq!(
            run(
                copy(false, false, &["/a/f", "/b/f"], "/d"),
                &fs,
                &Recorder::new()
            ),
            shown(&CpError::JustCreated {
                source: String::from("/b/f"),
                target: String::from("/d/f"),
            })
        );
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
            shown(&CpError::JustCreated {
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
        assert!(!fs.is_second_leaf_name("/dst/two", "/dst/one"));
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
        assert!(!fs.is_second_leaf_name("/dst/two", "/dst/one"));
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
                &seams(&fs, &NeverAsked, &OneDoc, &out, &out)
            ),
            Ok(true)
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

    /// GNU 9.4: `cp f f2 g` with no `g` is `target 'g': No such file or
    /// directory`, and with `g` a file `target 'g': Not a directory`.
    #[test]
    fn several_sources_need_a_directory_target() {
        let fs = MemFs::new().file("/a", b"x").file("/b", b"y");
        let out = Recorder::new();
        assert_eq!(
            run(copy(false, false, &["/a", "/b"], "/a"), &fs, &out),
            shown(&target_error("/a", false, Errno::NotADirectory))
        );
        assert_eq!(
            run(copy(false, false, &["/a", "/b"], "/c"), &fs, &out),
            shown(&target_error("/c", false, Errno::NotFound))
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
            shown(&CpError::IsDirectory(String::from("/d")))
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
            shown(&CpError::Overwrite {
                source: String::from("/d"),
                target: String::from("/e")
            })
        );
    }

    #[test]
    fn a_missing_source_fails_closed() {
        let fs = MemFs::new();
        let out = Recorder::new();
        assert_eq!(
            run(copy(false, false, &["/absent"], "/dst"), &fs, &out),
            shown(&stat_error("/absent", Errno::NotFound))
        );
    }

    /// GNU 9.4: `cp a/x missing b/y e/` names the missing one, still copies
    /// `b/y` and exits 1.
    #[test]
    fn a_failed_source_is_reported_and_the_next_still_copied() {
        let fs = MemFs::new().file("/b", b"present").dir("/dst");
        let out = Recorder::new();
        assert_eq!(
            run(copy(false, false, &["/absent", "/b"], "/dst"), &fs, &out),
            shown(&stat_error("/absent", Errno::NotFound))
        );
        assert_eq!(fs.contents("/dst/b").as_deref(), Some(&b"present"[..]));
    }

    #[test]
    fn an_unreadable_source_surfaces_the_errno() {
        let fs = MemFs::new()
            .file("/a", b"data")
            .read_fails("/a", Errno::PermissionDenied);
        let out = Recorder::new();
        assert_eq!(
            run(copy(false, false, &["/a"], "/b"), &fs, &out),
            shown(&read_error("/a", Errno::PermissionDenied))
        );
    }

    #[test]
    fn an_entry_that_fails_is_reported_and_the_rest_of_its_directory_still_copied() {
        let fs = MemFs::new()
            .dir("/s")
            .file("/s/a", b"a")
            .file("/s/b", b"b")
            .dir("/s/c")
            .file("/s/c/d", b"d")
            .dir("/dst")
            .read_fails("/s/a", Errno::PermissionDenied);
        assert_eq!(
            run(copy(true, false, &["/s"], "/dst"), &fs, &Recorder::new()),
            shown(&read_error("/s/a", Errno::PermissionDenied))
        );
        assert_eq!(fs.contents("/dst/s/b").as_deref(), Some(&b"b"[..]));
        assert_eq!(fs.contents("/dst/s/c/d").as_deref(), Some(&b"d"[..]));
    }

    #[test]
    fn a_directory_that_failed_leaves_no_ancestry_for_the_next_operand() {
        // `-L` follows `/s2/up` back to `/s1`, whose own copy ended when its
        // listing failed: reaching it again is no copy into itself.
        let fs = MemFs::new()
            .dir("/s1")
            .dir("/s2")
            .symlink_at("/s2/up", "/s1")
            .dir("/dst")
            .read_fails("/s1", Errno::PermissionDenied);
        let follow = Options {
            recursive: true,
            dereference: Dereference::Always,
            ..Options::DEFAULT
        };
        assert_eq!(
            run(
                copy_with(follow, &["/s1", "/s2"], "/dst"),
                &fs,
                &Recorder::new()
            ),
            Err(format!(
                "cp: {}\ncp: {}\n",
                read_error("/s1", Errno::PermissionDenied),
                read_error("/s2/up", Errno::PermissionDenied)
            ))
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
            shown(&create_error("/b", Errno::PermissionDenied))
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
            err,
            format!(
                "cp: cannot create '/b'$'\\033': {}\n",
                Errno::PermissionDenied
            )
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
            shown(&write_error("/b", Errno::PermissionDenied))
        );
    }

    #[test]
    fn help_output_failure_propagates() {
        let fs = MemFs::new();
        let out = Recorder::failing();
        assert_eq!(
            run(Command::Help, &fs, &out),
            shown(&CpError::Output(Errno::NotFound))
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
        assert_eq!(fs.tree.paths(), ["/", "/dst"], "nothing was made");
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
    fn an_entry_listed_as_a_file_and_turned_directory_leaves_the_rest_in_place() {
        let fs = MemFs::new()
            .file("/src/d1/x", b"x")
            .file("/src/z", b"z")
            .dir("/dst");
        fs.tree.before_line(
            "open_file /src/d1/x",
            Box::new(|tree: &Tree| {
                tree.remove("/src/d1/x");
                let made = tree
                    .open_dir("/src/d1", false)
                    .and_then(|d1| d1.make_dir("x", 0o755));
                assert!(made.is_ok());
            }),
        );
        let refused = CpError::Read {
            path: String::from("/src/d1/x"),
            errno: Errno::IsADirectory,
        };
        assert_eq!(
            run(copy(true, false, &["/src"], "/dst"), &fs, &Recorder::new()),
            shown(&refused)
        );
        assert_eq!(fs.contents("/dst/src/z"), Some(b"z".to_vec()));
        assert!(!fs.tree.exists("/dst/src/d1/z"));
        assert!(!fs.tree.exists("/dst/src/d1/x"));
    }

    #[test]
    fn a_link_followed_inside_a_tree_is_followed_from_the_directory_holding_it() {
        let fs = MemFs::new()
            .file("/src/real", b"mine")
            .symlink_at("/src/l", "real")
            .file("/evil/real", b"theirs")
            .symlink_at("/evil/l", "real")
            .dir("/out");
        // Once the walk holds the source, its name is another's directory.
        fs.tree.before_line(
            "entries /src",
            Box::new(|tree: &Tree| {
                tree.rename("/src", "/src-was");
                tree.rename("/evil", "/src");
            }),
        );
        let every = copy_with(
            Options {
                recursive: true,
                dereference: Dereference::Always,
                ..Options::DEFAULT
            },
            &["/src"],
            "/out",
        );
        assert_eq!(run(every, &fs, &Recorder::new()), Ok(()));
        assert_eq!(fs.contents("/out/src/l"), Some(b"mine".to_vec()));
    }

    #[test]
    fn an_existing_destination_inside_a_tree_need_only_be_writable() {
        let fs = MemFs::new()
            .file("/src/f", b"new")
            .file("/dst/src/f", b"old")
            .with_mode("/dst/src/f", 0o200)
            .with(Tree::checking_permissions);
        assert_eq!(
            run(copy(true, false, &["/src"], "/dst"), &fs, &Recorder::new()),
            Ok(())
        );
        assert_eq!(fs.contents("/dst/src/f"), Some(b"new".to_vec()));
    }

    #[test]
    fn a_link_inside_a_destination_tree_is_never_written_through() {
        let fs = MemFs::new()
            .file("/src/f", b"new")
            .file("/victim", b"kept")
            .dir("/dst/src")
            .symlink_at("/dst/src/f", "/victim");
        assert_eq!(
            run(copy(true, false, &["/src"], "/dst"), &fs, &Recorder::new()),
            shown(&CpError::ThroughLink(String::from("/dst/src/f")))
        );
        assert_eq!(fs.contents("/victim"), Some(b"kept".to_vec()));
    }

    #[test]
    fn an_occupant_swapped_after_consent_is_not_the_one_replaced() {
        let fs = MemFs::new()
            .file("/src/a", b"a")
            .file("/dst/src/a", b"agreed");
        fs.tree.before_line(
            "unlink /dst/src/a",
            Box::new(|tree: &Tree| {
                tree.remove("/dst/src/a");
                tree.write("/dst/src/a", b"swapped in");
            }),
        );
        let prompt = Answers::new(&[true]);
        let out = Recorder::new();
        assert_eq!(
            engine_run(
                copy_with(
                    Options {
                        recursive: true,
                        contents: Contents::SymbolicLink,
                        force: true,
                        clobber: Clobber::Prompt,
                        ..Options::DEFAULT
                    },
                    &["/src"],
                    "/dst",
                ),
                None,
                &seams(&fs, &prompt, &NoHelp, &out, &out),
            ),
            Ok(false)
        );
        assert_eq!(fs.contents("/dst/src/a"), Some(b"swapped in".to_vec()));
    }

    #[test]
    fn directories_a_walk_ended_early_had_made_take_their_modes() {
        let unlisted = MemFs::new()
            .dir("/src")
            .with_mode("/src", 0o555)
            .dir("/dst")
            .with(|tree| tree.refuse("entries /src", Errno::PermissionDenied));
        let _ = run(
            copy(true, false, &["/src"], "/dst"),
            &unlisted,
            &Recorder::new(),
        );
        assert_eq!(unlisted.tree.mode("/dst/src"), Some(0o555));
        let cut = MemFs::new()
            .dir("/src/a")
            .file("/src/a/f", b"f")
            .with_mode("/src/a", 0o555)
            .dir("/dst");
        let verbose = copy_with(
            Options {
                recursive: true,
                verbose: true,
                ..Options::DEFAULT
            },
            &["/src"],
            "/dst",
        );
        let out = Recorder::failing_after(2);
        assert!(engine_run(
            verbose,
            None,
            &seams(&cut, &NeverAsked, &NoHelp, &out, &out)
        )
        .is_err());
        assert_eq!(cut.tree.mode("/dst/src/a"), Some(0o555));
    }

    /// A tree to copy, the victims outside it, and the links an attacker
    /// swaps into it.
    fn bait_tree() -> MemFs {
        MemFs::new()
            .file("/src/f", b"F")
            .file("/src/sub/g", b"G")
            .file("/victim", b"victim")
            .file("/vdir/x", b"x")
            .symlink_at("/bait-f", "/victim")
            .symlink_at("/bait-sub", "/vdir")
            .dir("/dst")
    }

    /// Swap each name a copy reads or writes, as it stands, for a link to a
    /// victim.
    fn swap_names(tree: &Tree) {
        for (name, bait) in [
            ("/src/f", "/bait-f"),
            ("/src/sub", "/bait-sub"),
            ("/dst/src/sub", "/bait-sub"),
        ] {
            if tree.exists(name) && tree.exists(bait) {
                tree.rename(name, &format!("{name}.gone"));
                tree.rename(bait, name);
            }
        }
    }

    #[test]
    fn a_name_swapped_for_a_link_at_any_point_never_leads_a_copy_out() {
        let clean = bait_tree();
        let _ = run(
            copy(true, false, &["/src"], "/dst"),
            &clean,
            &Recorder::new(),
        );
        for n in 0..clean.tree.calls().len() {
            let fs = bait_tree();
            fs.tree.before_call(n, Box::new(swap_names));
            let _ = run(copy(true, false, &["/src"], "/dst"), &fs, &Recorder::new());
            assert_eq!(fs.contents("/victim"), Some(b"victim".to_vec()), "call {n}");
            assert_eq!(fs.contents("/vdir/x"), Some(b"x".to_vec()), "call {n}");
            assert_eq!(
                fs.tree
                    .paths()
                    .iter()
                    .filter(|path| path.starts_with("/vdir/"))
                    .count(),
                1,
                "nothing was made through the link, call {n}"
            );
        }
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
                &seams(&fs, &prompt, &NoHelp, &out, &out),
            ),
            Ok(false)
        );
        // `/dst/a` was confirmed and overwritten; `/dst/c` was declined and
        // kept, which ends the run failed with nothing to say, as GNU 9.4's.
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
                &seams(&fs, &prompt, &NoHelp, &out, &out),
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
            shown(&CpError::Target {
                path: String::from("/blocker"),
                directory: true,
                errno: Errno::NotADirectory
            })
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
            shown(&target_error("/absent", true, Errno::NotFound))
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
            shown(&CpError::Usage)
        );
    }
}
