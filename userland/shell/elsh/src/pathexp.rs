//! Filename generation (bash's pathname expansion): a word holding an
//! unquoted wildcard stands for the paths it matches, sorted.
//!
//! The word is compiled by the shared matcher in pathname mode; a root it
//! spells in literal text (`/`, `Home:/`, `alias::Home/`, `id::…/`) is
//! recognised by the shared path grammar and kept as written, and the rest
//! is walked a component at a time:
//!
//! * a literal component is appended, its existence checked at the end of
//!   the word, without following a final link, so a dangling one still
//!   matches as in bash, or by the listing of a later one;
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
use core::cell::Cell;
use core::ops::ControlFlow;

use tairix_abi::{Errno, FileKind};
use tairix_glob::{Component, Mark, Options, OutOfMemory, Pattern, Scratch, MAX_PATTERN_LEN};

use crate::complete::{DirEntryInfo, DirLister};
use crate::env::Expansion;
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
/// whole: compiled once for each value and the options it is read under,
/// and kept for the session.
#[derive(Default)]
pub(crate) struct Ignore {
    compiled: Cell<Option<Compiled>>,
}

impl Ignore {
    /// Remove from `paths` those an entry of `text` matches — read as bash
    /// reads them, against the whole path and a wildcard reaching a leading
    /// `.`, with `word`'s extended and case options — and, as bash does
    /// whenever it filters, any whose last component is `.` or `..`. A value
    /// not yet compiled under those options is compiled first, charged to
    /// `budget` at what it holds.
    ///
    /// # Errors
    ///
    /// An entry that is not a valid pattern or whose group holds a `/`, named
    /// as the variable; the bound spent compiling one; or a match the heap
    /// could not hold, which leaves what to remove unknown.
    pub(crate) fn retain(
        &self,
        text: &str,
        word: Options,
        paths: &mut Vec<String>,
        budget: &mut Budget,
    ) -> Result<(), ExpandError> {
        let options = word.with(Options::PERIOD, false);
        let compiled = match self.compiled.take() {
            Some(compiled) if compiled.options == options && compiled.text == text => compiled,
            stale => {
                drop(stale);
                Compiled::new(text, options, budget)?
            }
        };
        let kept = compiled.retain(paths);
        self.compiled.set(Some(compiled));
        kept
    }
}

/// One `GLOBIGNORE` value, compiled, or the refusal of its first entry that
/// is no pattern it can hold.
struct Compiled {
    text: String,
    options: Options,
    patterns: Result<Vec<Pattern>, ParseError>,
}

impl Compiled {
    fn new(text: &str, options: Options, budget: &mut Budget) -> Result<Self, ParseError> {
        let mut kept = budget.string(text.len())?;
        kept.push_str(text);
        let mut patterns = Vec::new();
        for entry in text.split(':').filter(|entry| !entry.is_empty()) {
            let pattern = match Pattern::with_options(entry, options) {
                // Compared a component at a time, an entry whose group holds a
                // `/` could hide less than it names.
                Ok(pattern) if pattern.components().len() <= entry.matches('/').count() => {
                    Err(ParseError::PatternGroupHoldsSlash)
                }
                Ok(pattern) => Ok(pattern),
                Err(err) => Err(ParseError::Pattern(err)),
            };
            match pattern {
                Ok(pattern) => {
                    budget.spend(pattern.heap_bytes())?;
                    budget.push(&mut patterns, pattern)?;
                }
                Err(refusal) => {
                    return Ok(Self {
                        text: kept,
                        options,
                        patterns: Err(refusal),
                    });
                }
            }
        }
        Ok(Self {
            text: kept,
            options,
            patterns: Ok(patterns),
        })
    }

    fn retain(&self, paths: &mut Vec<String>) -> Result<(), ExpandError> {
        let patterns = self.patterns.as_ref().map_err(|refusal| ExpandError {
            error: *refusal,
            subject: Some(String::from("GLOBIGNORE")),
        })?;
        let mut scratch = Scratch::new();
        let mut refused = None;
        paths.retain(|path| {
            if refused.is_some() {
                return true;
            }
            let last = match path.rsplit_once('/') {
                Some((_, last)) if !last.is_empty() => last,
                _ => path,
            };
            if last == "." || last == ".." {
                return false;
            }
            match ignores(patterns, path, &mut scratch) {
                Ok(ignored) => !ignored,
                Err(err) => {
                    refused = Some(err);
                    true
                }
            }
        });
        refused.map_or(Ok(()), |err| Err(err.into()))
    }
}

fn ignores(patterns: &[Pattern], path: &str, scratch: &mut Scratch) -> Result<bool, OutOfMemory> {
    for pattern in patterns {
        if pattern.matches_with(path, scratch)? {
            return Ok(true);
        }
    }
    Ok(false)
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

fn naming(path: &str, error: ParseError) -> ExpandError {
    ExpandError {
        error,
        subject: Some(String::from(path)),
    }
}

/// A path found so far.
#[derive(Clone)]
struct Found {
    path: String,
    /// The components after its root; with none yet, the next name needs no
    /// separator.
    components: usize,
}

impl Found {
    fn join(&self, name: &str) -> Self {
        let mut path = String::with_capacity(self.path.len() + 1 + name.len());
        path.push_str(&self.path);
        if self.components > 0 {
            path.push('/');
        }
        path.push_str(name);
        Self {
            path,
            components: self.components + 1,
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

/// What removes, from the paths a pattern matched, those that must not be
/// kept.
pub(crate) type MatchFilter<'a> =
    dyn Fn(&mut Vec<String>, &mut Budget) -> Result<(), ExpandError> + 'a;

/// The sorted paths `word` matches when it holds a wildcard, or `Ok(None)`
/// when it holds none and stands for itself. `filter` removes what it must
/// from the paths as bash spells them, before any is written from `./`.
pub(crate) fn expand(
    word: &Expansion,
    settings: &Settings,
    fs: &dyn GlobFs,
    budget: &mut Budget,
    filter: &MatchFilter<'_>,
) -> Result<Option<Vec<String>>, ExpandError> {
    let extended = settings.word.contains(Options::EXTENDED);
    let wildcard = |ch| tairix_glob::may_hold_wildcards(&[Mark { ch, literal: false }], extended);
    if !word.unquoted_chars().any(wildcard) {
        return Ok(None);
    }
    // A text longer than this is too long a pattern however it goes on, so
    // no more of it need be marked.
    let marks: Vec<Mark> = word.marks().take(MAX_PATTERN_LEN + 1).collect();
    let pattern = Pattern::from_marked(&marks, settings.word).map_err(|err| ExpandError {
        error: ParseError::Pattern(err),
        subject: Some(word.text.clone()),
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
        Some(first) if first.contains(':') => match tairix_path::root_len(&word.text) {
            Ok(len) => (
                String::from(word.text.get(..len).unwrap_or_default()),
                &all[1..],
            ),
            // No path spelling at all (a resource reference such as
            // `sys:*`): nothing on disk can match it.
            Err(_) => return Ok(Some(Vec::new())),
        },
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
        components: 0,
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
    let mut paths = walker.budget.list(found.len())?;
    // `**` standing for no level of the working directory is no path.
    for mut path in found
        .into_iter()
        .map(|entry| entry.path)
        .filter(|path| !path.is_empty())
    {
        if dirs_only {
            walker.budget.room(&mut path, 1)?;
            path.push('/');
        }
        paths.push(path);
    }
    paths.sort_unstable();
    paths.dedup();
    filter(&mut paths, walker.budget)?;
    if relative {
        for path in &mut paths {
            if !tairix_path::is_relative_spelling(path) {
                walker.budget.room(path, "./".len())?;
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

/// A `**` walk in progress: what it yielded, and the levels still to read.
struct Descent {
    next: Vec<Found>,
    pending: Vec<Found>,
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
        let mut next = Vec::new();
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
        if !component.matches_with(name, &mut self.scratch)? {
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
                walker.level_entry(&base, name, kind, yields, &mut walk)
            })?;
            if !listed {
                continue;
            }
            // The level `**` adds nothing to, spelled with the separator it
            // followed (`dir/**` names `dir/`).
            match yields {
                Yields::Everything if base.components > 0 => {
                    let mut itself = base;
                    self.budget.room(&mut itself.path, 1)?;
                    itself.path.push('/');
                    self.keep(itself, &mut walk.next)?;
                }
                // A level is charged as it is read, kept or not, so the
                // walk stays within the expansion bound.
                Yields::Matches(..) => self.charge(&base)?,
                _ => self.keep(base, &mut walk.next)?,
            }
            while let Some(dir) = walk.pending.pop() {
                self.read(&dir, |walker, name, kind| {
                    walker.level_entry(&dir, name, kind, yields, &mut walk)
                })?;
            }
        }
        Ok(walk.next)
    }

    /// Take one entry of a level of a `**` walk: what it yields, and — a
    /// subdirectory, never a link — a level to read next.
    fn level_entry(
        &mut self,
        dir: &Found,
        name: &str,
        kind: FileKind,
        yields: Yields<'_>,
        walk: &mut Descent,
    ) -> Result<(), ExpandError> {
        if let Yields::Matches(component, need_dir) = yields {
            self.match_entry(component, dir, name, kind, need_dir, &mut walk.next)?;
        }
        if !self.settings.dotglob() && tairix_path::is_hidden_name(name) {
            return Ok(());
        }
        // A path through a deeper level could not be spelled, so the walk
        // ends there, wherever it began.
        let is_level =
            kind == FileKind::Directory && dir.components + 1 < tairix_path::MAX_COMPONENTS;
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
                self.keep(child, &mut walk.pending)?;
            }
            (true, false) => self.keep(child, &mut walk.pending)?,
            (false, true) => self.keep(child, &mut walk.next)?,
            (false, false) => {}
        }
        Ok(())
    }

    fn charge(&mut self, found: &Found) -> Result<(), ExpandError> {
        Ok(self.budget.spend(found.path.len())?)
    }

    fn keep(&mut self, found: Found, into: &mut Vec<Found>) -> Result<(), ExpandError> {
        self.charge(&found)?;
        Ok(self.budget.push(into, found)?)
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
            Err(errno) => Err(naming(&listing, ParseError::UnreadableDirectory(errno))),
        }
    }

    /// Whether `found` names a node — a directory, when `need_dir`, a final
    /// link then followed to see.
    fn exists(&self, found: &Found, need_dir: bool) -> Result<bool, ExpandError> {
        let path = spelled(&found.path, self.relative);
        match self.fs.kind(&path, need_dir) {
            Ok(kind) => Ok(!need_dir || kind.is_dir()),
            Err(errno) if absent(errno) => Ok(false),
            Err(errno) => Err(naming(&path, ParseError::UnresolvablePath(errno))),
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
