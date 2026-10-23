//! Filename generation (bash's pathname expansion): a word holding an
//! unquoted wildcard stands for the paths it matches, sorted.
//!
//! The word is compiled by the shared matcher in pathname mode; a root it
//! spells in literal text (`/`, `Home:/`, `alias::Home/`, `id::…/`) is
//! recognised by the shared path grammar and kept as written, and the rest
//! is walked a component at a time:
//!
//! * a literal component is appended, its existence checked at the end of
//!   the word (as bash's `lstat` does) or by the listing of a later one;
//! * a wildcard component reads its directory a batch at a time and keeps
//!   the names it matches, a leading `.` matched only by a `.` unless
//!   `dotglob`, so no listing is ever held whole;
//! * a component before the last, or before a trailing `/`, needs a
//!   directory: one listed as such, or a link resolving to one;
//! * under `globstar`, a `**` component (or a run of them) stands for the
//!   directory itself and every directory below it, never descending through
//!   a link; a wildcard after it is matched against the listings that walk
//!   reads, so no directory is read twice.
//!
//! Matches sort bytewise — the C collation every listing on the system uses
//! — and [`Ignore`] then removes those `GLOBIGNORE` names. A relative path
//! that a listed name leads is written from `./` when that name holds a `:`,
//! which the path grammar would otherwise read as a root.

use alloc::borrow::Cow;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::ops::ControlFlow;

use tairix_abi::{Errno, FileKind};
use tairix_glob::{Component, Mark, Options, Pattern, Scratch};

use crate::complete::{DirEntryInfo, DirLister};
use crate::error::ParseError;
use crate::expand::{Budget, ExpandError};

/// The read-only view of the filesystem filename generation walks: a
/// directory's entries, read as they arrive with links reported as links,
/// and the kind of the node a path names. Completion lists through the same
/// view ([`DirLister`]).
///
/// On a running kernel both are kernel-authorised (`fs_readdir`, and a
/// resolve-only open's `fs_stat`), so a pattern never reveals a name the
/// caller could not list itself; in tests it is an in-memory tree.
pub trait GlobFs: DirLister {
    /// Hand each entry of the directory `dir` to `each` as it is read — its
    /// name and kind — until the listing ends or `each` breaks.
    ///
    /// # Errors
    ///
    /// The host's [`Errno`] when `dir` cannot be listed, also once some of
    /// its entries were handed over.
    fn read_dir(
        &self,
        dir: &str,
        each: &mut dyn FnMut(&str, FileKind) -> ControlFlow<()>,
    ) -> Result<(), Errno>;

    /// The kind of the node `path` names, a final link followed when
    /// `follow` and reported as a link otherwise.
    ///
    /// # Errors
    ///
    /// The host's [`Errno`]: `NotFound` for no such node.
    fn kind(&self, path: &str, follow: bool) -> Result<FileKind, Errno>;
}

/// The fail-closed default: nothing can be read, so no pattern matches.
pub(crate) struct NullFs;

impl DirLister for NullFs {
    fn list_dir(&self, _dir: &str) -> Result<Vec<DirEntryInfo>, Errno> {
        Err(Errno::NotImplemented)
    }
}

impl GlobFs for NullFs {
    fn read_dir(
        &self,
        _dir: &str,
        _each: &mut dyn FnMut(&str, FileKind) -> ControlFlow<()>,
    ) -> Result<(), Errno> {
        Err(Errno::NotImplemented)
    }

    fn kind(&self, _path: &str, _follow: bool) -> Result<FileKind, Errno> {
        Err(Errno::NotImplemented)
    }
}

/// The shared fail-closed default filesystem view (see [`NullFs`]).
pub(crate) static NULL_FS: NullFs = NullFs;

/// How filename generation reads patterns, from the shell's options.
pub(crate) struct Settings {
    /// How a word's own pattern compiles: extended under `extglob`,
    /// case-insensitive under `nocaseglob`, and with the leading-period rule
    /// unless `dotglob` (or a non-empty `GLOBIGNORE`, which implies it).
    pub(crate) word: Options,
    pub(crate) globstar: bool,
}

impl Settings {
    /// How a word's pattern compiles from the options in force.
    pub(crate) fn word_options(extglob: bool, nocaseglob: bool, dotglob: bool) -> Options {
        Options::PATHNAME
            .with(Options::EXTENDED, extglob)
            .with(Options::CASE_INSENSITIVE, nocaseglob)
            .with(Options::PERIOD, !dotglob)
    }

    fn dotglob(&self) -> bool {
        !self.word.contains(Options::PERIOD)
    }
}

/// `GLOBIGNORE`'s `:`-separated patterns, which remove the paths they match
/// whole.
pub(crate) struct Ignore(Vec<Pattern>);

impl Ignore {
    /// Compile `text`'s entries as bash matches them: against the whole path,
    /// a wildcard reaching a leading `.`.
    ///
    /// # Errors
    ///
    /// An entry that is not a valid pattern, named as the variable.
    pub(crate) fn new(text: &str, extglob: bool, nocaseglob: bool) -> Result<Self, ExpandError> {
        let options = Settings::word_options(extglob, nocaseglob, true);
        text.split(':')
            .filter(|entry| !entry.is_empty())
            .map(|entry| {
                Pattern::with_options(entry, options).map_err(|err| ExpandError {
                    error: ParseError::Pattern(err),
                    subject: Some(String::from("GLOBIGNORE")),
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Self)
    }

    /// Remove the paths an entry matches, and — as bash does whenever it
    /// filters — any whose last component is `.` or `..`.
    pub(crate) fn retain(&self, paths: &mut Vec<String>) {
        let mut scratch = Scratch::new();
        paths.retain(|path| {
            let last = match path.rsplit_once('/') {
                Some((_, last)) if !last.is_empty() => last,
                _ => path,
            };
            last != "."
                && last != ".."
                && !self
                    .0
                    .iter()
                    .any(|pattern| pattern.matches_with(path, &mut scratch))
        });
    }
}

/// A listing or probe failing this way means *nothing here*, as bash reads
/// it: the path is absent; is no directory that can be listed — not a
/// directory, a link that resolves nowhere, a node or view that lists
/// nothing; lies beyond what a path can reach, too deep or too long for the
/// kernel to resolve; or is not the caller's to see. Any other failure — a
/// volume gone, a device failing — would make the result silently partial,
/// so it is reported instead.
fn absent(errno: Errno) -> bool {
    matches!(
        errno,
        Errno::NotFound
            | Errno::NotADirectory
            | Errno::PermissionDenied
            | Errno::LinkLoop
            | Errno::OutOfRange
            | Errno::LengthOutOfRange
            | Errno::NotImplemented
            | Errno::NotSupported
    )
}

fn unreadable(path: &str, errno: Errno) -> ExpandError {
    ExpandError {
        error: ParseError::UnreadableDirectory(errno),
        subject: Some(String::from(path)),
    }
}

/// A path found so far.
#[derive(Clone)]
struct Found {
    path: String,
    /// Nothing follows the root yet, so the next name needs no separator.
    at_root: bool,
}

impl Found {
    fn join(&self, name: &str) -> Self {
        let mut path = String::with_capacity(self.path.len() + 1 + name.len());
        path.push_str(&self.path);
        if !self.at_root {
            path.push('/');
        }
        path.push_str(name);
        Self {
            path,
            at_root: false,
        }
    }

    /// The spelling to list, as [`spelled`]: the working directory for an
    /// empty relative root.
    fn listing(&self, relative: bool) -> Cow<'_, str> {
        if self.path.is_empty() {
            Cow::Borrowed(".")
        } else {
            spelled(&self.path, relative)
        }
    }
}

/// `path` as the filesystem is asked for it: a relative one whose leading
/// name holds a `:` — read as a root otherwise — written from `./`.
fn spelled(path: &str, relative: bool) -> Cow<'_, str> {
    if relative && !tairix_path::is_relative_spelling(path) {
        Cow::Owned(format!("./{path}"))
    } else {
        Cow::Borrowed(path)
    }
}

/// The sorted paths `word` matches when it holds a wildcard, or `Ok(None)`
/// when it holds none and stands for itself. `filter` removes what it must
/// from the paths as bash spells them, before any is written from `./`.
pub(crate) fn expand(
    word: &[Mark],
    settings: &Settings,
    fs: &dyn GlobFs,
    budget: &mut Budget,
    filter: &dyn Fn(&mut Vec<String>) -> Result<(), ExpandError>,
) -> Result<Option<Vec<String>>, ExpandError> {
    if !tairix_glob::may_hold_wildcards(word, settings.word.contains(Options::EXTENDED)) {
        return Ok(None);
    }
    let text = || -> String { word.iter().map(|mark| mark.ch).collect() };
    let pattern = Pattern::from_marked(word, settings.word).map_err(|err| ExpandError {
        error: ParseError::Pattern(err),
        subject: Some(text()),
    })?;
    if !pattern.has_wildcards() {
        return Ok(None);
    }
    let all = pattern.components();
    // A root is spelled in literal text before the first `/`: a `:` there is
    // the path grammar's delimiter, while one inside a wildcard component is
    // pattern syntax (`[[:digit:]]`).
    let (root, mut components) = match all.first().and_then(Component::literal) {
        Some("") => (String::from("/"), &all[1..]),
        Some(first) if first.contains(':') => {
            let mut text = text();
            match tairix_path::root_len(&text) {
                Ok(len) => {
                    text.truncate(len);
                    (text, &all[1..])
                }
                // No path spelling at all (a resource reference such as
                // `sys:*`): nothing on disk can match it.
                Err(_) => return Ok(Some(Vec::new())),
            }
        }
        _ => (String::new(), all),
    };
    let dirs_only =
        components.len() > 1 && components.last().and_then(Component::literal) == Some("");
    if dirs_only {
        components = &components[..components.len() - 1];
    }
    let relative = root.is_empty();
    let mut walker = Walker {
        fs,
        settings,
        scratch: Scratch::new(),
        budget,
        relative,
    };
    let mut found = vec![Found {
        path: root,
        at_root: true,
    }];
    let mut rest = components;
    while let Some((component, after)) = rest.split_first() {
        rest = after;
        found = if settings.globstar && component.is_double_star() {
            // A run of `**` stands for the levels one does.
            while rest.first().is_some_and(Component::is_double_star) {
                rest = &rest[1..];
            }
            let yields = match rest.split_first() {
                None if dirs_only => Yields::Directories,
                None => Yields::Everything,
                Some((next, after)) if next.literal().is_none() => {
                    rest = after;
                    Yields::Matches(next, !rest.is_empty() || dirs_only)
                }
                Some(_) => Yields::Levels,
            };
            walker.globstar(found, yields)?
        } else {
            walker.step(component, &found, rest.is_empty(), dirs_only)?
        };
        if found.is_empty() {
            break;
        }
    }
    let mut paths: Vec<String> = found
        .into_iter()
        // `**` standing for no level of the working directory is no path.
        .filter(|entry| !entry.path.is_empty())
        .map(|mut entry| {
            if dirs_only {
                entry.path.push('/');
            }
            entry.path
        })
        .collect();
    paths.sort_unstable();
    paths.dedup();
    filter(&mut paths)?;
    if relative {
        for path in &mut paths {
            if !tairix_path::is_relative_spelling(path) {
                walker.budget.spend("./".len())?;
                path.insert_str(0, "./");
            }
        }
    }
    Ok(Some(paths))
}

/// What a `**` walk yields from the levels it reads: the base directory and
/// every directory below it, links never descended.
#[derive(Clone, Copy)]
enum Yields<'c> {
    /// The levels themselves, for a literal component after `**`.
    Levels,
    /// A final `**/`: every directory, and every link resolving to one.
    Directories,
    /// A final `**`: everything.
    Everything,
    /// What the wildcard component after `**` matches in each level, a
    /// directory or a link to one when the flag says it must be.
    Matches(&'c Component, bool),
}

/// A `**` walk in progress: what it yielded, and the levels still to read
/// with their depth.
struct Descent {
    next: Vec<Found>,
    pending: Vec<(Found, usize)>,
}

struct Walker<'a, 'b> {
    fs: &'a dyn GlobFs,
    settings: &'a Settings,
    scratch: Scratch,
    budget: &'b mut Budget,
    /// The word spells no root, so a listed name may lead a path.
    relative: bool,
}

impl Walker<'_, '_> {
    /// The paths one more component reaches from `found`. `last` marks the
    /// final component, and `dirs_only` a trailing `/` after it.
    fn step(
        &mut self,
        component: &Component,
        found: &[Found],
        last: bool,
        dirs_only: bool,
    ) -> Result<Vec<Found>, ExpandError> {
        let need_dir = !last || dirs_only;
        let mut next = Vec::with_capacity(found.len());
        if let Some(name) = component.literal() {
            for base in found {
                let path = base.join(name);
                if !last || self.exists(&path, need_dir)? {
                    self.keep(path, &mut next)?;
                }
            }
            return Ok(next);
        }
        for base in found {
            self.read(base, |walker, name, kind| {
                walker.match_entry(component, base, name, kind, need_dir, &mut next)
            })?;
        }
        Ok(next)
    }

    /// Keep `dir`'s entry `name` when `component` matches it: only a
    /// directory, or a link resolving to one, when `need_dir`.
    fn match_entry(
        &mut self,
        component: &Component,
        dir: &Found,
        name: &str,
        kind: FileKind,
        need_dir: bool,
        into: &mut Vec<Found>,
    ) -> Result<(), ExpandError> {
        if !component.matches_with(name, &mut self.scratch) {
            return Ok(());
        }
        let path = dir.join(name);
        if !need_dir || self.is_directory(kind, &path)? {
            self.keep(path, into)?;
        }
        Ok(())
    }

    /// `**` from each base that is a directory: what `yields` asks of the
    /// base and of every directory below it, each read once.
    fn globstar(
        &mut self,
        found: Vec<Found>,
        yields: Yields<'_>,
    ) -> Result<Vec<Found>, ExpandError> {
        let mut walk = Descent {
            next: Vec::new(),
            pending: Vec::new(),
        };
        for base in found {
            let listed = self.read(&base, |walker, name, kind| {
                walker.level_entry(&base, name, kind, 0, yields, &mut walk)
            })?;
            if !listed {
                continue;
            }
            // The level `**` adds nothing to, spelled with the separator it
            // followed (`dir/**` names `dir/`).
            match yields {
                Yields::Everything if !base.at_root => {
                    let mut itself = base;
                    itself.path.push('/');
                    self.keep(itself, &mut walk.next)?;
                }
                // A level is charged as it is read, kept or not, so the
                // walk stays within the expansion bound.
                Yields::Matches(..) => self.charge(&base)?,
                _ => self.keep(base, &mut walk.next)?,
            }
            while let Some((dir, depth)) = walk.pending.pop() {
                self.read(&dir, |walker, name, kind| {
                    walker.level_entry(&dir, name, kind, depth, yields, &mut walk)
                })?;
            }
        }
        Ok(walk.next)
    }

    /// Take one entry of a level `depth` below a `**` walk's base: what it
    /// yields, and — a subdirectory, never a link — a level to read next.
    fn level_entry(
        &mut self,
        dir: &Found,
        name: &str,
        kind: FileKind,
        depth: usize,
        yields: Yields<'_>,
        walk: &mut Descent,
    ) -> Result<(), ExpandError> {
        if let Yields::Matches(component, need_dir) = yields {
            self.match_entry(component, dir, name, kind, need_dir, &mut walk.next)?;
        }
        if !self.settings.dotglob() && tairix_path::is_hidden_name(name) {
            return Ok(());
        }
        let below = depth + 1;
        // A deeper level could not be spelled, so the walk ends there.
        let is_level = kind == FileKind::Directory && below < tairix_path::MAX_COMPONENTS;
        let may_yield = matches!(yields, Yields::Directories | Yields::Everything);
        if !is_level && !may_yield {
            return Ok(());
        }
        let child = dir.join(name);
        let yielded = match yields {
            Yields::Levels => is_level,
            Yields::Directories => self.is_directory(kind, &child)?,
            Yields::Everything => true,
            Yields::Matches(..) => false,
        };
        match (is_level, yielded) {
            (true, true) => {
                self.keep(child.clone(), &mut walk.next)?;
                walk.pending.push((child, below));
            }
            (true, false) => {
                self.charge(&child)?;
                walk.pending.push((child, below));
            }
            (false, true) => self.keep(child, &mut walk.next)?,
            (false, false) => {}
        }
        Ok(())
    }

    fn charge(&mut self, found: &Found) -> Result<(), ExpandError> {
        Ok(self.budget.charge(found.path.len())?)
    }

    fn keep(&mut self, found: Found, into: &mut Vec<Found>) -> Result<(), ExpandError> {
        self.charge(&found)?;
        into.push(found);
        Ok(())
    }

    /// Read `dir`'s listing through `visit` an entry at a time, never `.` or
    /// `..`, which a listing naming them is not taken at its word on.
    /// `false` when `dir` is not there to list; an error naming it when the
    /// listing failed otherwise, or failed once it had begun, since what it
    /// had read would be only part.
    fn read(
        &mut self,
        dir: &Found,
        mut visit: impl FnMut(&mut Self, &str, FileKind) -> Result<(), ExpandError>,
    ) -> Result<bool, ExpandError> {
        let fs = self.fs;
        let listing = dir.listing(self.relative);
        let mut began = false;
        let mut failed = None;
        let read = fs.read_dir(&listing, &mut |name, kind| {
            began = true;
            if name == "." || name == ".." {
                return ControlFlow::Continue(());
            }
            match visit(self, name, kind) {
                Ok(()) => ControlFlow::Continue(()),
                Err(err) => {
                    failed = Some(err);
                    ControlFlow::Break(())
                }
            }
        });
        if let Some(err) = failed {
            return Err(err);
        }
        match read {
            Ok(()) => Ok(true),
            Err(errno) if absent(errno) && !began => Ok(false),
            Err(errno) => Err(unreadable(&listing, errno)),
        }
    }

    /// Whether `found` names a node — a directory, when `need_dir`, a final
    /// link then followed to see.
    fn exists(&self, found: &Found, need_dir: bool) -> Result<bool, ExpandError> {
        let path = spelled(&found.path, self.relative);
        match self.fs.kind(&path, need_dir) {
            Ok(kind) => Ok(!need_dir || kind.is_dir()),
            Err(errno) if absent(errno) => Ok(false),
            Err(errno) => Err(unreadable(&path, errno)),
        }
    }

    /// Whether an entry of `kind` at `found` is a directory or a link
    /// resolving to one.
    fn is_directory(&self, kind: FileKind, found: &Found) -> Result<bool, ExpandError> {
        match kind {
            FileKind::Directory => Ok(true),
            FileKind::Symlink => self.exists(found, true),
            FileKind::Regular => Ok(false),
        }
    }
}
