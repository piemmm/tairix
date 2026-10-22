//! Tests for the move engine, driven against an in-memory filesystem and a
//! recording output.

use super::{run as engine_run, Seams, USAGE};
use crate::command::{Clobber, Command, Options, TargetMode};
use crate::error::MvError;
use crate::io::{
    Ancestry, AttributeStore, Entry, EntryKind, FileSystem, Output, Owner, PathFiles, Prompt,
    RenameOutcome,
};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::RefCell;
use tairix_abi::fs::SeekRegion;
use tairix_abi::{Errno, FileId, RenameFlags};
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

    fn read(&self, _locale_dir: &str, _file_name: &str) -> Result<Option<Vec<u8>>, SourceError> {
        Ok(None)
    }
}

/// A Help tree holding one canonical `mv.md` document.
struct OneDoc;

const DOC: &str = "## NAME\n\nmv — move files and directories\n\n\
                   ## SYNOPSIS\n\n`mv [-f] [--] source... dest`\n\n\
                   ## DESCRIPTION\n\nMoves things.\n";

impl HelpSource for OneDoc {
    fn locale_dirs(&self) -> Result<Vec<String>, SourceError> {
        Ok(alloc::vec![String::from("en-US")])
    }

    fn read(&self, locale_dir: &str, file_name: &str) -> Result<Option<Vec<u8>>, SourceError> {
        if locale_dir == "en-US" && file_name == "mv.md" {
            Ok(Some(DOC.as_bytes().to_vec()))
        } else {
            Ok(None)
        }
    }
}

/// The engine under a prompt that must never be reached — the shape every
/// pre-existing non-interactive test uses.
fn run(command: Command, fs: &dyn FileSystem, out: &Recorder) -> Result<(), MvError> {
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

/// An in-memory tree. Regular files carry their bytes; directories are named
/// so their children can be derived by parent path. Failure injection covers
/// the rename/read/create/write/remove fail-closed paths, and a cross-device
/// prefix forces the copy-then-remove relocation.
struct MemFs {
    state: RefCell<State>,
}

struct State {
    files: Vec<(String, Vec<u8>)>,
    dirs: Vec<String>,
    /// A `source` under this path prefix renames cross-device.
    cross_device: Option<String>,
    /// `rename` with this source fails with the errno.
    rename_fail: Option<(String, Errno)>,
    /// When set, `rename` fails with [`Errno::PermissionDenied`] while the
    /// destination still exists, modelling a destination that blocks an
    /// atomic rename until it is removed (the `-f` recovery path).
    block_if_dest_exists: bool,
    /// `read` of this path fails with the errno.
    read_fail: Option<(String, Errno)>,
    /// `write` to this path fails with the errno.
    write_fail: Option<(String, Errno)>,
    /// `remove_file`/`remove_dir` of this path fails with the errno.
    remove_fail: Option<(String, Errno)>,
    /// A file another program creates at its path just before the next
    /// rename reaches it — after any look the client took.
    appears_at_rename: Option<(String, Vec<u8>)>,
    /// Symbolic links: `(path, stored target)`, the target verbatim.
    symlinks: Vec<(String, String)>,
    /// The stored runs `(start, end)` of a sparse file; a file not listed
    /// is stored throughout.
    runs: Vec<(String, Vec<(u64, u64)>)>,
    /// Every write's `(path, length)`, in order.
    writes: Vec<(String, usize)>,
    /// Extended attributes: `(path, key, value)`.
    attrs: Vec<(String, String, Vec<u8>)>,
    /// Setting this key anywhere is refused with the errno.
    attr_refusal: Option<(String, Errno)>,
    /// Owners and permission bits set on a path; one not listed has its
    /// kind's default, owned by [`MOVER`].
    owners: Vec<(String, Owner)>,
    /// Whether the mover may give a node to someone else.
    may_give_away: bool,
    /// Staging names drawn so far.
    staged: u32,
    /// `sync` of this path fails with the errno.
    sync_fail: Option<(String, Errno)>,
    /// What changed the tree, in order: `sync`, `rename`, `remove`.
    events: Vec<String>,
}

/// Who the fixture's process is: a node it makes is theirs.
const MOVER: (u32, u32) = (7, 7);

impl MemFs {
    fn new() -> Self {
        Self {
            state: RefCell::new(State {
                files: Vec::new(),
                dirs: Vec::new(),
                cross_device: None,
                rename_fail: None,
                block_if_dest_exists: false,
                read_fail: None,
                write_fail: None,
                remove_fail: None,
                appears_at_rename: None,
                symlinks: Vec::new(),
                runs: Vec::new(),
                writes: Vec::new(),
                attrs: Vec::new(),
                attr_refusal: None,
                owners: Vec::new(),
                may_give_away: false,
                staged: 0,
                sync_fail: None,
                events: Vec::new(),
            }),
        }
    }

    /// `path` owned by `owner`.
    fn owned(self, path: &str, owner: Owner) -> Self {
        self.state
            .borrow_mut()
            .owners
            .push((path.to_string(), owner));
        self
    }

    /// A mover with the authority to give a node away.
    fn may_give_away(self) -> Self {
        self.state.borrow_mut().may_give_away = true;
        self
    }

    fn sync_fails(self, path: &str, errno: Errno) -> Self {
        self.state.borrow_mut().sync_fail = Some((path.to_string(), errno));
        self
    }

    fn events(&self) -> Vec<String> {
        self.state.borrow().events.clone()
    }

    /// The name the next staging draw returns for `target`.
    fn staging_for(&self, target: &str) -> String {
        format!(
            "{}.stage{}.new",
            canon(target),
            self.state.borrow().staged + 1
        )
    }

    /// Every path the tree holds under `prefix`, the prefix itself included.
    fn paths_under(&self, prefix: &str) -> Vec<String> {
        let state = self.state.borrow();
        let inside = |p: &str| p == prefix || p.starts_with(&format!("{prefix}/"));
        let mut paths: Vec<String> = state
            .files
            .iter()
            .map(|(p, _)| p.clone())
            .chain(state.dirs.iter().cloned())
            .chain(state.symlinks.iter().map(|(p, _)| p.clone()))
            .filter(|p| inside(p))
            .collect();
        paths.sort();
        paths
    }

    /// FNV-1a over the path: stable for a path and distinct between paths,
    /// however the tree around it changes.
    fn identity_of(&self, path: &str) -> Option<FileId> {
        let state = self.state.borrow();
        if state.symlinks.iter().any(|(p, _)| p == path) {
            return Some(FileId::NONE);
        }
        let known = path == "/"
            || state.files.iter().any(|(p, _)| p == path)
            || state.dirs.iter().any(|p| p == path);
        known.then(|| FileId {
            volume: [3; 16],
            node: path.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
                (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
            }) | 1,
        })
    }

    /// A symbolic link at `path` storing `target` verbatim.
    fn symlink_at(self, path: &str, target: &str) -> Self {
        self.state
            .borrow_mut()
            .symlinks
            .push((path.to_string(), target.to_string()));
        self
    }

    /// A sparse file at `path`, `len` bytes long, storing `runs`.
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

    fn attr(self, path: &str, key: &str, value: &[u8]) -> Self {
        self.state
            .borrow_mut()
            .attrs
            .push((path.to_string(), key.to_string(), value.to_vec()));
        self
    }

    fn refuses_attr(self, key: &str, errno: Errno) -> Self {
        self.state.borrow_mut().attr_refusal = Some((key.to_string(), errno));
        self
    }

    fn attr_of(&self, path: &str, key: &str) -> Option<Vec<u8>> {
        self.state
            .borrow()
            .attrs
            .iter()
            .find(|(p, k, _)| p == canon(path) && k == key)
            .map(|(_, _, value)| value.clone())
    }

    fn link_target(&self, path: &str) -> Option<String> {
        self.state
            .borrow()
            .symlinks
            .iter()
            .find(|(p, _)| p == canon(path))
            .map(|(_, target)| target.clone())
    }

    fn written_to(&self, path: &str) -> usize {
        self.state
            .borrow()
            .writes
            .iter()
            .filter(|(p, _)| p == canon(path))
            .map(|(_, len)| len)
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

    fn cross_device(self, prefix: &str) -> Self {
        self.state.borrow_mut().cross_device = Some(prefix.to_string());
        self
    }

    fn rename_fails(self, source: &str, errno: Errno) -> Self {
        self.state.borrow_mut().rename_fail = Some((source.to_string(), errno));
        self
    }

    fn appears_at_rename(self, path: &str, contents: &[u8]) -> Self {
        self.state.borrow_mut().appears_at_rename = Some((path.to_string(), contents.to_vec()));
        self
    }

    fn blocks_until_dest_removed(self) -> Self {
        self.state.borrow_mut().block_if_dest_exists = true;
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

    fn remove_fails(self, path: &str, errno: Errno) -> Self {
        self.state.borrow_mut().remove_fail = Some((path.to_string(), errno));
        self
    }

    fn contents(&self, path: &str) -> Option<Vec<u8>> {
        self.state
            .borrow()
            .files
            .iter()
            .find(|(p, _)| p == canon(path))
            .map(|(_, bytes)| bytes.clone())
    }

    fn has_dir(&self, path: &str) -> bool {
        self.state.borrow().dirs.iter().any(|p| p == canon(path))
    }

    fn exists(&self, path: &str) -> bool {
        self.contents(path).is_some() || self.has_dir(path) || self.link_target(path).is_some()
    }
}

/// Canonicalise a path: a trailing slash on anything but the root names the
/// same object (`/d/` is `/d`).
fn canon(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        "/"
    } else {
        trimmed
    }
}

/// The immediate parent of a path: `/a/b` maps to `/a`, and a top-level `/a`
/// maps to the empty string (its children sit at the root level).
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

impl PathFiles for MemFs {
    fn read(&self, path: &str, offset: u64, buf: &mut [u8]) -> Result<usize, Errno> {
        let path = canon(path);
        let state = self.state.borrow();
        if let Some((p, errno)) = &state.read_fail {
            if p == path {
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
        self.contents(path)
            .map(|bytes| bytes.len() as u64)
            .ok_or(Errno::NotFound)
    }
    fn next_region(
        &self,
        path: &str,
        offset: u64,
        region: SeekRegion,
    ) -> Result<Option<u64>, Errno> {
        let len = self.size(path)?;
        let state = self.state.borrow();
        let Some((_, runs)) = state.runs.iter().find(|(p, _)| p == canon(path)) else {
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
        let mut state = self.state.borrow_mut();
        let (_, file) = state
            .files
            .iter_mut()
            .find(|(p, _)| p == canon(path))
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
        state.writes.push((path.to_string(), bytes.len()));
        Ok(())
    }
}

impl FileSystem for MemFs {
    fn kind(&self, path: &str) -> Result<EntryKind, Errno> {
        let path = canon(path);
        let state = self.state.borrow();
        if state.dirs.iter().any(|p| p == path) {
            return Ok(EntryKind::Directory);
        }
        if state.files.iter().any(|(p, _)| p == path) {
            return Ok(EntryKind::File);
        }
        if state.symlinks.iter().any(|(p, _)| p == path) {
            return Ok(EntryKind::Symlink);
        }
        Err(Errno::NotFound)
    }

    fn resolved_kind(&self, path: &str) -> Result<EntryKind, Errno> {
        let mut name = canon(path).to_string();
        for _ in 0..8 {
            match self.kind(&name)? {
                EntryKind::Symlink => name = self.link_target(&name).ok_or(Errno::NotFound)?,
                kind => return Ok(kind),
            }
        }
        Err(Errno::NotFound)
    }

    fn read_link(&self, path: &str) -> Result<String, Errno> {
        self.link_target(path).ok_or(Errno::OutOfRange)
    }

    fn symlink(&self, target: &str, link: &str) -> Result<(), Errno> {
        if self.exists(link) {
            return Err(Errno::AlreadyExists);
        }
        self.state
            .borrow_mut()
            .symlinks
            .push((canon(link).to_string(), target.to_string()));
        Ok(())
    }

    /// The kernel's rename rules: a kind replaces only its own kind, a
    /// directory only an empty one, and never one inside itself.
    fn rename(&self, source: &str, dest: &str, flags: RenameFlags) -> Result<RenameOutcome, Errno> {
        let source = canon(source);
        let dest = canon(dest);
        {
            let mut state = self.state.borrow_mut();
            if let Some(planted) = state.appears_at_rename.take() {
                state.files.push(planted);
            }
            if let Some((p, errno)) = &state.rename_fail {
                if p == source {
                    return Err(*errno);
                }
            }
            if let Some(prefix) = &state.cross_device {
                if source.starts_with(prefix.as_str()) {
                    return Ok(RenameOutcome::CrossDevice);
                }
            }
        }
        let moving = self.kind(source)?;
        let occupant = self.kind(dest).ok();
        if occupant.is_some() && flags.refuses_replace() {
            return Err(Errno::AlreadyExists);
        }
        if self.state.borrow().block_if_dest_exists && occupant.is_some() {
            return Err(Errno::PermissionDenied);
        }
        if moving == EntryKind::Directory
            && (dest == source || dest.starts_with(&format!("{source}/")))
        {
            return Err(Errno::OutOfRange);
        }
        match (moving, occupant) {
            (EntryKind::Directory, Some(EntryKind::Directory)) => {
                if self.paths_under(dest).len() > 1 {
                    return Err(Errno::NotEmpty);
                }
            }
            (EntryKind::Directory, Some(_)) => return Err(Errno::NotADirectory),
            (_, Some(EntryKind::Directory)) => return Err(Errno::IsADirectory),
            _ => {}
        }
        let mut state = self.state.borrow_mut();
        state.events.push(format!("rename {source} -> {dest}"));
        state.files.retain(|(p, _)| p != dest);
        state.dirs.retain(|p| p != dest);
        state.symlinks.retain(|(p, _)| p != dest);
        state.owners.retain(|(p, _)| p != dest);
        state.attrs.retain(|(p, _, _)| p != dest);
        let prefix = format!("{source}/");
        let repath = |p: &mut String| {
            if p == source {
                *p = dest.to_string();
            } else if let Some(rest) = p.strip_prefix(&prefix) {
                *p = format!("{dest}/{rest}");
            }
        };
        let state = &mut *state;
        state.files.iter_mut().for_each(|(p, _)| repath(p));
        state.dirs.iter_mut().for_each(repath);
        state.symlinks.iter_mut().for_each(|(p, _)| repath(p));
        state.owners.iter_mut().for_each(|(p, _)| repath(p));
        state.attrs.iter_mut().for_each(|(p, _, _)| repath(p));
        Ok(RenameOutcome::Renamed)
    }

    fn read_dir(&self, path: &str, index: u64) -> Result<Option<Entry>, Errno> {
        let path = canon(path);
        let state = self.state.borrow();
        if !state.dirs.iter().any(|p| p == path) {
            return Err(Errno::NotFound);
        }
        let mut entries = Vec::new();
        for (p, _) in &state.files {
            if parent_of(p) == path {
                entries.push(Entry {
                    name: name_of(p).to_string(),
                    kind: EntryKind::File,
                });
            }
        }
        for p in &state.dirs {
            if parent_of(p) == path {
                entries.push(Entry {
                    name: name_of(p).to_string(),
                    kind: EntryKind::Directory,
                });
            }
        }
        for (p, _) in &state.symlinks {
            if parent_of(p) == path {
                entries.push(Entry {
                    name: name_of(p).to_string(),
                    kind: EntryKind::Symlink,
                });
            }
        }
        let idx = usize::try_from(index).map_err(|_| Errno::LengthOutOfRange)?;
        Ok(entries.into_iter().nth(idx))
    }

    fn mkdir(&self, path: &str) -> Result<(), Errno> {
        if self.exists(path) {
            return Err(Errno::AlreadyExists);
        }
        self.state.borrow_mut().dirs.push(canon(path).to_string());
        Ok(())
    }

    fn create(&self, path: &str) -> Result<(), Errno> {
        if self.exists(path) {
            return Err(Errno::AlreadyExists);
        }
        self.state
            .borrow_mut()
            .files
            .push((canon(path).to_string(), Vec::new()));
        Ok(())
    }

    fn remove_file(&self, path: &str) -> Result<(), Errno> {
        let path = canon(path);
        let mut state = self.state.borrow_mut();
        if let Some((p, errno)) = &state.remove_fail {
            if p == path {
                return Err(*errno);
            }
        }
        // A directory is not a file: unlinking one is refused, as the
        // kernel refuses it.
        if state.dirs.iter().any(|p| p == path) {
            return Err(Errno::IsADirectory);
        }
        state.events.push(format!("remove {path}"));
        state.files.retain(|(p, _)| p != path);
        state.symlinks.retain(|(p, _)| p != path);
        state.owners.retain(|(p, _)| p != path);
        Ok(())
    }

    fn remove_dir(&self, path: &str) -> Result<(), Errno> {
        let path = canon(path);
        if self.paths_under(path).len() > 1 {
            return Err(Errno::NotEmpty);
        }
        let mut state = self.state.borrow_mut();
        if let Some((p, errno)) = &state.remove_fail {
            if p == path {
                return Err(*errno);
            }
        }
        state.events.push(format!("remove {path}"));
        state.dirs.retain(|p| p != path);
        state.owners.retain(|(p, _)| p != path);
        Ok(())
    }

    fn owner(&self, path: &str) -> Result<Owner, Errno> {
        let kind = self.kind(path)?;
        let state = self.state.borrow();
        Ok(state.owners.iter().find(|(p, _)| p == canon(path)).map_or(
            Owner {
                mode: if kind == EntryKind::Directory {
                    0o755
                } else {
                    0o644
                },
                uid: MOVER.0,
                gid: MOVER.1,
            },
            |(_, owner)| *owner,
        ))
    }

    fn set_mode(&self, path: &str, mode: u32) -> Result<(), Errno> {
        let current = self.owner(path)?;
        self.set(path, Owner { mode, ..current });
        Ok(())
    }

    fn set_owner(&self, path: &str, uid: u32, gid: u32) -> Result<(), Errno> {
        let current = self.owner(path)?;
        if (uid, gid) != MOVER && !self.state.borrow().may_give_away {
            return Err(Errno::PermissionDenied);
        }
        self.set(
            path,
            Owner {
                uid,
                gid,
                ..current
            },
        );
        Ok(())
    }

    fn staging_name(&self, target: &str) -> Result<String, Errno> {
        let name = self.staging_for(target);
        self.state.borrow_mut().staged += 1;
        Ok(name)
    }

    fn sync(&self, path: &str) -> Result<(), Errno> {
        let path = canon(path);
        let mut state = self.state.borrow_mut();
        if let Some((p, errno)) = &state.sync_fail {
            if p == path {
                return Err(*errno);
            }
        }
        state.events.push(format!("sync {path}"));
        Ok(())
    }
}

impl MemFs {
    fn set(&self, path: &str, owner: Owner) {
        let mut state = self.state.borrow_mut();
        state.owners.retain(|(p, _)| p != canon(path));
        state.owners.push((canon(path).to_string(), owner));
    }
}

impl Ancestry for MemFs {
    fn canonical(&self, path: &str) -> Result<String, Errno> {
        tairix_fscopy::fixture::resolve(path, |here| self.link_target(here))
    }

    fn identity(&self, path: &str) -> Result<FileId, Errno> {
        self.identity_of(canon(path)).ok_or(Errno::NotFound)
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
        self.attr_of(path, key).ok_or(Errno::NoData)
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

/// Captures every byte written; optionally fails on the first write.
struct Recorder {
    bytes: RefCell<Vec<u8>>,
    fail: bool,
}

impl Recorder {
    fn new() -> Self {
        Self {
            bytes: RefCell::new(Vec::new()),
            fail: false,
        }
    }

    fn failing() -> Self {
        Self {
            bytes: RefCell::new(Vec::new()),
            fail: true,
        }
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
}

fn mv_with(options: Options, sources: &[&str], dest: &str) -> Command {
    Command::Move {
        options,
        sources: sources.iter().map(|p| (*p).to_string()).collect::<Vec<_>>(),
        dest: dest.to_string(),
    }
}

fn mv(no_clobber: bool, sources: &[&str], dest: &str) -> Command {
    mv_with(
        Options {
            clobber: if no_clobber {
                Clobber::Skip
            } else {
                Clobber::Overwrite
            },
            ..Options::DEFAULT
        },
        sources,
        dest,
    )
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
    assert!(text.contains("mv — move files and directories"), "{text}");
    assert!(text.contains("mv [-f] [--] source... dest"), "{text}");
}

#[test]
fn renames_a_file_to_a_new_path() {
    let fs = MemFs::new().file("/a.txt", b"hello");
    let out = Recorder::new();
    assert_eq!(run(mv(false, &["/a.txt"], "/b.txt"), &fs, &out), Ok(()));
    assert_eq!(fs.contents("/b.txt").as_deref(), Some(&b"hello"[..]));
    // The source no longer exists; `mv` is silent on success.
    assert!(!fs.exists("/a.txt"));
    assert_eq!(out.text(), "");
}

#[test]
fn renames_a_directory() {
    let fs = MemFs::new().dir("/d").file("/d/f", b"x");
    let out = Recorder::new();
    assert_eq!(run(mv(false, &["/d"], "/e"), &fs, &out), Ok(()));
    assert!(fs.has_dir("/e"));
    assert!(!fs.has_dir("/d"));
    assert_eq!(fs.contents("/e/f").as_deref(), Some(&b"x"[..]));
}

#[test]
fn moves_a_file_into_an_existing_directory_under_its_basename() {
    let fs = MemFs::new().file("/src/a.txt", b"data").dir("/dst");
    let out = Recorder::new();
    assert_eq!(run(mv(false, &["/src/a.txt"], "/dst"), &fs, &out), Ok(()));
    assert_eq!(fs.contents("/dst/a.txt").as_deref(), Some(&b"data"[..]));
    assert!(!fs.exists("/src/a.txt"));
}

#[test]
fn moves_several_files_into_a_directory() {
    let fs = MemFs::new()
        .file("/a", b"one")
        .file("/b", b"two")
        .dir("/dst");
    let out = Recorder::new();
    assert_eq!(run(mv(false, &["/a", "/b"], "/dst"), &fs, &out), Ok(()));
    assert_eq!(fs.contents("/dst/a").as_deref(), Some(&b"one"[..]));
    assert_eq!(fs.contents("/dst/b").as_deref(), Some(&b"two"[..]));
}

#[test]
fn several_sources_to_a_non_directory_dest_is_usage() {
    let fs = MemFs::new().file("/a", b"x").file("/b", b"y");
    let out = Recorder::new();
    assert_eq!(
        run(mv(false, &["/a", "/b"], "/c"), &fs, &out),
        Err(MvError::Usage)
    );
    assert!(!fs.exists("/c"));
}

#[test]
fn a_missing_source_fails_closed() {
    let fs = MemFs::new();
    let out = Recorder::new();
    assert_eq!(
        run(mv(false, &["/absent"], "/dst"), &fs, &out),
        Err(MvError::Stat {
            path: String::from("/absent"),
            errno: Errno::NotFound
        })
    );
}

#[test]
fn a_failure_stops_before_a_later_source() {
    let fs = MemFs::new().file("/b", b"present").dir("/dst");
    let out = Recorder::new();
    assert_eq!(
        run(mv(false, &["/absent", "/b"], "/dst"), &fs, &out),
        Err(MvError::Stat {
            path: String::from("/absent"),
            errno: Errno::NotFound
        })
    );
    // The second source was never moved.
    assert_eq!(fs.contents("/b").as_deref(), Some(&b"present"[..]));
    assert!(!fs.exists("/dst/b"));
}

#[test]
fn no_clobber_skips_an_existing_destination() {
    let fs = MemFs::new().file("/a", b"new").file("/b", b"old");
    let out = Recorder::new();
    assert_eq!(run(mv(true, &["/a"], "/b"), &fs, &out), Ok(()));
    // The destination is untouched and the source remains.
    assert_eq!(fs.contents("/b").as_deref(), Some(&b"old"[..]));
    assert_eq!(fs.contents("/a").as_deref(), Some(&b"new"[..]));
}

/// `-n` decides on what it saw, so a destination created between its look
/// and its rename must not be destroyed: the kernel refuses the move and the
/// source is skipped as if the file had been there all along.
#[test]
fn no_clobber_never_replaces_a_destination_created_after_its_look() {
    let fs = MemFs::new()
        .file("/a", b"new")
        .appears_at_rename("/b", b"theirs");
    let out = Recorder::new();
    assert_eq!(run(mv(true, &["/a"], "/b"), &fs, &out), Ok(()));
    assert_eq!(fs.contents("/b").as_deref(), Some(&b"theirs"[..]));
    assert_eq!(fs.contents("/a").as_deref(), Some(&b"new"[..]));
}

/// `-i` asks before replacing anything, including a destination that only
/// appeared after its look.
#[test]
fn interactive_asks_about_a_destination_created_after_its_look() {
    for (reply, kept) in [(false, &b"theirs"[..]), (true, &b"new"[..])] {
        let fs = MemFs::new()
            .file("/a", b"new")
            .appears_at_rename("/b", b"theirs");
        let out = Recorder::new();
        let prompt = Answers::new(&[reply]);
        let command = mv_with(
            Options {
                clobber: Clobber::Prompt,
                ..Options::DEFAULT
            },
            &["/a"],
            "/b",
        );
        assert_eq!(
            engine_run(command, None, seams(&fs, &prompt, &NoHelp, &out, &out)),
            Ok(())
        );
        assert_eq!(fs.contents("/b").as_deref(), Some(kept));
        assert_eq!(prompt.asked(), ["overwrite '/b'?"]);
    }
}

#[test]
fn overwrites_an_existing_destination_by_default() {
    let fs = MemFs::new().file("/a", b"new").file("/b", b"old");
    let out = Recorder::new();
    assert_eq!(run(mv(false, &["/a"], "/b"), &fs, &out), Ok(()));
    assert_eq!(fs.contents("/b").as_deref(), Some(&b"new"[..]));
    assert!(!fs.exists("/a"));
}

#[test]
fn a_failed_rename_surfaces_the_errno() {
    let fs = MemFs::new()
        .file("/a", b"x")
        .rename_fails("/a", Errno::PermissionDenied);
    let out = Recorder::new();
    assert_eq!(
        run(mv(false, &["/a"], "/b"), &fs, &out),
        Err(MvError::Rename {
            source: String::from("/a"),
            target: String::from("/b"),
            errno: Errno::PermissionDenied
        })
    );
}

#[test]
fn a_destination_that_blocks_the_rename_is_kept_with_the_source() {
    let fs = MemFs::new()
        .file("/a", b"fresh")
        .file("/b", b"stale")
        .blocks_until_dest_removed();
    let out = Recorder::new();
    assert_eq!(
        run(mv(false, &["/a"], "/b"), &fs, &out),
        Err(MvError::Rename {
            source: String::from("/a"),
            target: String::from("/b"),
            errno: Errno::PermissionDenied
        })
    );
    // Nothing moved: the source and the stale destination both remain.
    assert_eq!(fs.contents("/a").as_deref(), Some(&b"fresh"[..]));
    assert_eq!(fs.contents("/b").as_deref(), Some(&b"stale"[..]));
}

#[test]
fn force_only_stops_the_questions_and_never_destroys_a_destination_of_another_kind() {
    // GNU's `-f` is "do not prompt": a directory still cannot replace a file,
    // and the file is not removed to make room.
    let fs = MemFs::new()
        .dir("/d")
        .file("/d/f", b"inside")
        .file("/b", b"kept");
    let forced = Options {
        clobber: Clobber::Overwrite,
        target_mode: TargetMode::NoDirectory,
        ..Options::DEFAULT
    };
    assert_eq!(
        run(mv_with(forced, &["/d"], "/b"), &fs, &Recorder::new()),
        Err(MvError::OverwriteNonDirectory {
            source: String::from("/d"),
            target: String::from("/b"),
        })
    );
    assert_eq!(fs.contents("/b").as_deref(), Some(&b"kept"[..]));
    assert_eq!(fs.contents("/d/f").as_deref(), Some(&b"inside"[..]));
}

#[test]
fn cross_device_file_is_copied_then_removed() {
    let fs = MemFs::new().file("/src/a", b"payload").cross_device("/src");
    let out = Recorder::new();
    assert_eq!(run(mv(false, &["/src/a"], "/dst"), &fs, &out), Ok(()));
    assert_eq!(fs.contents("/dst").as_deref(), Some(&b"payload"[..]));
    assert!(!fs.exists("/src/a"));
}

#[test]
fn cross_device_large_file_streams_across_the_chunk_boundary() {
    let payload: Vec<u8> = (0..(tairix_fscopy::CHUNK_LEN * 2 + 7))
        .map(|i| u8::try_from(i % 251).unwrap_or_default())
        .collect();
    let fs = MemFs::new().file("/src/big", &payload).cross_device("/src");
    let out = Recorder::new();
    assert_eq!(run(mv(false, &["/src/big"], "/copy"), &fs, &out), Ok(()));
    assert_eq!(fs.contents("/copy"), Some(payload));
    assert!(!fs.exists("/src/big"));
}

#[test]
fn cross_device_directory_is_reproduced_then_removed() {
    let fs = MemFs::new()
        .dir("/src/d")
        .file("/src/d/f", b"top")
        .dir("/src/d/sub")
        .file("/src/d/sub/g", b"nested")
        .cross_device("/src");
    let out = Recorder::new();
    assert_eq!(run(mv(false, &["/src/d"], "/e"), &fs, &out), Ok(()));
    assert!(fs.has_dir("/e"));
    assert!(fs.has_dir("/e/sub"));
    assert_eq!(fs.contents("/e/f").as_deref(), Some(&b"top"[..]));
    assert_eq!(fs.contents("/e/sub/g").as_deref(), Some(&b"nested"[..]));
    // The whole source subtree is gone.
    assert!(!fs.has_dir("/src/d"));
    assert!(!fs.has_dir("/src/d/sub"));
    assert!(!fs.exists("/src/d/f"));
    assert!(!fs.exists("/src/d/sub/g"));
}

#[test]
fn cross_device_read_failure_surfaces_the_errno() {
    let fs = MemFs::new()
        .file("/src/a", b"x")
        .cross_device("/src")
        .read_fails("/src/a", Errno::PermissionDenied);
    let out = Recorder::new();
    assert_eq!(
        run(mv(false, &["/src/a"], "/dst"), &fs, &out),
        Err(MvError::Read {
            path: String::from("/src/a"),
            errno: Errno::PermissionDenied
        })
    );
    // The source remains, because the copy failed before removal.
    assert!(fs.exists("/src/a"));
    assert!(
        fs.paths_under("/").iter().all(|p| !p.starts_with("/dst")),
        "a failed move leaves nothing made for it"
    );
}

#[test]
fn cross_device_write_failure_surfaces_the_errno() {
    let fs = MemFs::new()
        .file("/src/a", b"x")
        .cross_device("/src")
        .write_fails("/dst.stage1.new", Errno::PermissionDenied);
    let out = Recorder::new();
    assert_eq!(
        run(mv(false, &["/src/a"], "/dst"), &fs, &out),
        Err(MvError::Write {
            path: String::from("/dst"),
            errno: Errno::PermissionDenied
        })
    );
    assert!(fs.exists("/src/a"));
    assert!(
        fs.paths_under("/").iter().all(|p| !p.starts_with("/dst")),
        "a failed move leaves no partial file, staged or not"
    );
}

#[test]
fn an_attribute_key_holding_control_bytes_is_reported_escaped() {
    let refused = MvError::Attribute {
        key: String::from("user.\u{1b}[2J"),
        target: String::from("/dst"),
        error: Errno::Busy,
    };
    assert_eq!(
        refused.to_string(),
        alloc::format!(
            "setting attribute 'user.\\x1b[2J' for '/dst': {}",
            Errno::Busy
        )
    );
}

#[test]
fn cross_device_remove_failure_surfaces_the_errno() {
    let fs = MemFs::new()
        .file("/src/a", b"x")
        .cross_device("/src")
        .remove_fails("/src/a", Errno::PermissionDenied);
    let out = Recorder::new();
    assert_eq!(
        run(mv(false, &["/src/a"], "/dst"), &fs, &out),
        Err(MvError::Remove {
            path: String::from("/src/a"),
            errno: Errno::PermissionDenied
        })
    );
    // The destination already holds the copied data.
    assert_eq!(fs.contents("/dst").as_deref(), Some(&b"x"[..]));
}

#[test]
fn help_output_failure_propagates() {
    let fs = MemFs::new();
    let out = Recorder::failing();
    assert_eq!(
        run(Command::Help, &fs, &out),
        Err(MvError::Output(Errno::NotFound))
    );
}

#[test]
fn a_trailing_slash_source_moves_under_its_basename() {
    let fs = MemFs::new().dir("/d").file("/d/f", b"x").dir("/dst");
    let out = Recorder::new();
    assert_eq!(run(mv(false, &["/d/"], "/dst"), &fs, &out), Ok(()));
    assert!(fs.has_dir("/dst/d"));
    assert_eq!(fs.contents("/dst/d/f").as_deref(), Some(&b"x"[..]));
}

#[test]
fn an_alias_root_source_lands_under_its_own_root_spelling() {
    // Behaviour moved with the sweep onto the shared `leaf_name`: the private
    // base name this tool carried was not alias-aware and named `Home:/` as
    // `Home:`, dropping the separator that makes it a root.
    assert_eq!(tairix_path::leaf_name("Home:/"), "Home:/");
    // And an ordinary path is unchanged by the move.
    assert_eq!(tairix_path::leaf_name("/a/b/"), "b");
}

#[test]
fn an_empty_source_operand_names_nothing_and_fails_closed() {
    // The other moved answer: the private base name reported `/` for the
    // empty spelling, fabricating a root component the caller never typed.
    // `leaf_name` names nothing, and the move still fails on the unreachable
    // source rather than acting on a guessed path.
    assert_eq!(tairix_path::leaf_name(""), "");
    let fs = MemFs::new().dir("/dst");
    let out = Recorder::new();
    assert!(run(mv(false, &[""], "/dst"), &fs, &out).is_err());
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
            mv_with(
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
    // `/dst/a` was confirmed and replaced; `/dst/c` was declined and kept
    // (its source stays put), and the run still succeeds.
    assert_eq!(fs.contents("/dst/a").as_deref(), Some(&b"fresh"[..]));
    assert_eq!(fs.contents("/dst/c").as_deref(), Some(&b"blockC"[..]));
    assert_eq!(fs.contents("/c").as_deref(), Some(&b"old"[..]));
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
            mv_with(
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
        Err(MvError::Prompt(Errno::NotFound))
    );
    assert_eq!(fs.contents("/b").as_deref(), Some(&b"stale"[..]));
}

#[test]
fn verbose_reports_each_move_in_gnu_wording() {
    let fs = MemFs::new().file("/a", b"x").dir("/dst");
    let out = Recorder::new();
    assert_eq!(
        run(
            mv_with(
                Options {
                    verbose: true,
                    ..Options::DEFAULT
                },
                &["/a"],
                "/dst",
            ),
            &fs,
            &out,
        ),
        Ok(())
    );
    assert_eq!(out.text(), "renamed '/a' -> '/dst/a'\n");
}

#[test]
fn target_directory_moves_every_source_into_it() {
    let fs = MemFs::new()
        .file("/a", b"one")
        .file("/b", b"two")
        .dir("/dst");
    let out = Recorder::new();
    assert_eq!(
        run(
            mv_with(
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
            mv_with(
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
        Err(MvError::Usage)
    );
    assert_eq!(
        run(
            mv_with(
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
        Err(MvError::Stat {
            path: String::from("/absent"),
            errno: Errno::NotFound
        })
    );
}

#[test]
fn no_target_directory_refuses_several_sources() {
    let fs = MemFs::new().file("/a", b"x").file("/b", b"y").dir("/dst");
    let out = Recorder::new();
    assert_eq!(
        run(
            mv_with(
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
        Err(MvError::Usage)
    );
}

/// Run `command` with its diagnostics apart from its output.
fn run_apart(command: Command, fs: &dyn FileSystem) -> (Result<(), MvError>, Recorder, Recorder) {
    let (out, diagnostics) = (Recorder::new(), Recorder::new());
    let result = engine_run(
        command,
        None,
        seams(fs, &NeverAsked, &NoHelp, &out, &diagnostics),
    );
    (result, out, diagnostics)
}

#[test]
fn a_link_to_a_directory_moves_across_volumes_as_a_link_and_spares_its_target() {
    // The link is the object moved: what it names is neither copied nor, as
    // the source is removed, emptied.
    let fs = MemFs::new()
        .dir("/keep")
        .file("/keep/precious", b"do not lose")
        .dir("/src")
        .symlink_at("/src/shortcut", "/keep")
        .cross_device("/src");
    let (result, _, _) = run_apart(mv(false, &["/src/shortcut"], "/dst"), &fs);
    assert_eq!(result, Ok(()));
    assert_eq!(
        fs.link_target("/dst").as_deref(),
        Some("/keep"),
        "the link itself arrives"
    );
    assert!(!fs.exists("/src/shortcut"), "the source link is gone");
    assert_eq!(
        fs.contents("/keep/precious").as_deref(),
        Some(&b"do not lose"[..])
    );
    assert!(fs.has_dir("/keep"));
}

#[test]
fn links_inside_a_moved_directory_stay_links() {
    let fs = MemFs::new()
        .file("/elsewhere", b"target bytes")
        .dir("/src")
        .dir("/src/tree")
        .file("/src/tree/file", b"own bytes")
        .symlink_at("/src/tree/alias", "../elsewhere")
        .cross_device("/src");
    let (result, _, _) = run_apart(mv(false, &["/src/tree"], "/moved"), &fs);
    assert_eq!(result, Ok(()));
    assert_eq!(
        fs.contents("/moved/file").as_deref(),
        Some(&b"own bytes"[..])
    );
    assert_eq!(
        fs.link_target("/moved/alias").as_deref(),
        Some("../elsewhere"),
        "stored verbatim"
    );
    assert!(!fs.exists("/src/tree"));
    assert_eq!(
        fs.contents("/elsewhere").as_deref(),
        Some(&b"target bytes"[..])
    );
}

#[test]
fn a_move_across_volumes_carries_attributes_and_holes_and_reports_what_it_lost() {
    let fs = MemFs::new()
        .dir("/src")
        .sparse("/src/image", 256 * 1024, &[(4096, b"data")])
        .attr("/src/image", "acorn.filetype", b"fce")
        .attr("/src/image", "mac.type", b"BINA")
        .refuses_attr("mac.type", Errno::NotSupported)
        .cross_device("/src");
    let staged = fs.staging_for("/dst");
    let (result, out, diagnostics) = run_apart(mv(false, &["/src/image"], "/dst"), &fs);
    assert_eq!(result, Ok(()));
    assert_eq!(
        fs.contents("/dst").map(|bytes| bytes.len()),
        Some(256 * 1024)
    );
    assert_eq!(fs.written_to(&staged), 4, "the holes stay holes");
    assert_eq!(
        fs.attr_of("/dst", "acorn.filetype").as_deref(),
        Some(&b"fce"[..])
    );
    assert_eq!(
        diagnostics.text(),
        "mv: warning: extended attributes not kept on 1 file (the destination cannot store them): mac.type\n"
    );
    assert_eq!(out.text(), "");
    assert!(!fs.exists("/src/image"));
}

// --- moving across volumes: one replacement, made whole first ------------

#[test]
fn a_move_across_volumes_replaces_the_destination_only_once_the_copy_is_on_the_medium() {
    let fs = MemFs::new()
        .file("/src/a", b"new")
        .file("/dst", b"old")
        .cross_device("/src");
    let staged = fs.staging_for("/dst");
    assert_eq!(
        run(mv(false, &["/src/a"], "/dst"), &fs, &Recorder::new()),
        Ok(())
    );
    assert_eq!(fs.contents("/dst").as_deref(), Some(&b"new"[..]));
    assert!(!fs.exists("/src/a"));
    assert!(!fs.exists(&staged), "the staging name is gone");
    assert_eq!(
        fs.events(),
        alloc::vec![
            format!("sync {staged}"),
            format!("rename {staged} -> /dst"),
            String::from("sync /dst"),
            String::from("remove /src/a"),
        ],
        "on the medium before the rename, renamed before the source goes"
    );
}

#[test]
fn a_move_across_volumes_that_fails_keeps_the_destination_it_would_have_replaced() {
    let fs = MemFs::new()
        .file("/src/a", b"new")
        .file("/dst", b"old")
        .cross_device("/src");
    let staged = fs.staging_for("/dst");
    let fs = fs.write_fails(&staged, Errno::NoSpace);
    assert_eq!(
        run(mv(false, &["/src/a"], "/dst"), &fs, &Recorder::new()),
        Err(MvError::Write {
            path: String::from("/dst"),
            errno: Errno::NoSpace
        })
    );
    assert_eq!(fs.contents("/dst").as_deref(), Some(&b"old"[..]));
    assert_eq!(fs.contents("/src/a").as_deref(), Some(&b"new"[..]));
    assert!(!fs.exists(&staged));
}

/// GNU 9.4's refusal: the second operand would put `b/x` over the `a/x` the
/// first moved there, whose source is already gone.
#[test]
fn an_operand_never_replaces_what_an_earlier_one_moved_into_place() {
    for across in [false, true] {
        let fs = MemFs::new()
            .file("/src/a/x", b"first")
            .file("/src/b/x", b"second")
            .file("/src/b/y", b"third")
            .dir("/d");
        let fs = if across { fs.cross_device("/src") } else { fs };
        assert_eq!(
            run(
                mv(false, &["/src/a/x", "/src/b/x", "/src/b/y"], "/d"),
                &fs,
                &Recorder::new()
            ),
            Err(MvError::JustCreated {
                source: String::from("/src/b/x"),
                target: String::from("/d/x"),
            }),
            "across volumes: {across}"
        );
        assert_eq!(fs.contents("/d/x").as_deref(), Some(&b"first"[..]));
        assert_eq!(fs.contents("/src/b/x").as_deref(), Some(&b"second"[..]));
        assert!(!fs.exists("/src/a/x"));
    }
    let err = MvError::JustCreated {
        source: String::from("b/x"),
        target: String::from("d/x"),
    };
    assert_eq!(
        format!("{err}"),
        "will not overwrite just-created 'd/x' with 'b/x'"
    );
}

#[test]
fn a_directory_may_replace_the_empty_one_an_earlier_operand_moved_there() {
    let fs = MemFs::new().dir("/a/s").dir("/b/s").dir("/d");
    assert_eq!(
        run(mv(false, &["/a/s", "/b/s"], "/d"), &fs, &Recorder::new()),
        Ok(())
    );
    assert!(fs.exists("/d/s"));
    assert!(!fs.exists("/a/s") && !fs.exists("/b/s"));
}

#[test]
fn a_staging_name_another_holds_is_refused_and_left_alone() {
    for directory in [false, true] {
        let fs = MemFs::new().cross_device("/src");
        let fs = if directory {
            fs.dir("/src/a").file("/src/a/x", b"new")
        } else {
            fs.file("/src/a", b"new").file("/dst", b"old")
        };
        let taken = fs.staging_for("/dst");
        let fs = if directory {
            fs.dir(&taken).file(&format!("{taken}/theirs"), b"kept")
        } else {
            fs.file(&taken, b"kept")
        };
        let outcome = run(mv(false, &["/src/a"], "/dst"), &fs, &Recorder::new());
        assert_eq!(
            outcome,
            Err(MvError::Create {
                path: String::from("/dst"),
                errno: Errno::AlreadyExists
            }),
            "directory: {directory}"
        );
        let kept = if directory {
            format!("{taken}/theirs")
        } else {
            taken
        };
        assert_eq!(fs.contents(&kept).as_deref(), Some(&b"kept"[..]));
        if directory {
            assert!(!fs.exists("/dst"));
            assert_eq!(fs.contents("/src/a/x").as_deref(), Some(&b"new"[..]));
        } else {
            assert_eq!(fs.contents("/dst").as_deref(), Some(&b"old"[..]));
            assert_eq!(fs.contents("/src/a").as_deref(), Some(&b"new"[..]));
        }
    }
}

#[test]
fn a_move_across_volumes_whose_commit_fails_keeps_its_source() {
    let fs = MemFs::new()
        .file("/src/a", b"new")
        .cross_device("/src")
        .sync_fails("/dst", Errno::DeviceFault);
    assert_eq!(
        run(mv(false, &["/src/a"], "/dst"), &fs, &Recorder::new()),
        Err(MvError::Write {
            path: String::from("/dst"),
            errno: Errno::DeviceFault
        })
    );
    assert_eq!(fs.contents("/dst").as_deref(), Some(&b"new"[..]));
    assert_eq!(fs.contents("/src/a").as_deref(), Some(&b"new"[..]));
}

#[test]
fn a_directory_moved_across_volumes_keeps_the_renames_rules() {
    let tree = || {
        MemFs::new()
            .dir("/src/d")
            .file("/src/d/f", b"inside")
            .file("/src/file", b"plain")
            .dir("/full")
            .file("/full/keep", b"kept")
            .dir("/empty")
            .file("/afile", b"in the way")
            .cross_device("/src")
    };
    let onto = |source: &str, target: &str| {
        let fs = tree();
        let options = Options {
            target_mode: TargetMode::NoDirectory,
            ..Options::DEFAULT
        };
        let result = run(mv_with(options, &[source], target), &fs, &Recorder::new());
        (result, fs)
    };
    let (result, fs) = onto("/src/d", "/full");
    assert_eq!(
        result,
        Err(MvError::Rename {
            source: String::from("/src/d"),
            target: String::from("/full"),
            errno: Errno::NotEmpty
        })
    );
    assert_eq!(fs.contents("/full/keep").as_deref(), Some(&b"kept"[..]));
    assert!(fs.paths_under("/full").len() == 2, "nothing was copied in");
    let (result, fs) = onto("/src/d", "/afile");
    assert_eq!(
        result,
        Err(MvError::OverwriteNonDirectory {
            source: String::from("/src/d"),
            target: String::from("/afile"),
        })
    );
    assert_eq!(fs.contents("/afile").as_deref(), Some(&b"in the way"[..]));
    let (result, _) = onto("/src/file", "/empty");
    assert_eq!(
        result,
        Err(MvError::OverwriteDirectory {
            target: String::from("/empty"),
        })
    );
    let (result, fs) = onto("/src/d", "/empty");
    assert_eq!(result, Ok(()), "an empty directory is replaced");
    assert_eq!(fs.contents("/empty/f").as_deref(), Some(&b"inside"[..]));
    assert!(!fs.exists("/src/d"));
}

#[test]
fn a_directory_never_moves_into_its_own_subtree() {
    // Within a volume the kernel refuses it; across one (a volume attached
    // inside the source) the copy is refused before anything is written.
    let fs = MemFs::new().dir("/a").dir("/a/sub").file("/a/f", b"x");
    assert_eq!(
        run(mv(false, &["/a"], "/a/sub/in"), &fs, &Recorder::new()),
        Err(MvError::IntoItself {
            source: String::from("/a"),
            target: String::from("/a/sub/in"),
        })
    );
    let across = MemFs::new()
        .dir("/a")
        .dir("/a/mnt")
        .file("/a/f", b"x")
        .cross_device("/a");
    assert_eq!(
        run(mv(false, &["/a"], "/a/mnt/in"), &across, &Recorder::new()),
        Err(MvError::IntoItself {
            source: String::from("/a"),
            target: String::from("/a/mnt/in"),
        })
    );
    assert!(!across.exists("/a/mnt/in"));
    assert!(across.paths_under("/a/mnt").iter().all(|p| p == "/a/mnt"));
}

#[test]
fn a_moved_node_keeps_its_owner_and_mode_and_loses_set_id_bits_it_cannot_keep() {
    let theirs = Owner {
        mode: 0o4755,
        uid: 42,
        gid: 42,
    };
    let fs = MemFs::new()
        .file("/src/tool", b"bytes")
        .owned("/src/tool", theirs)
        .cross_device("/src");
    assert_eq!(
        run(mv(false, &["/src/tool"], "/tool"), &fs, &Recorder::new()),
        Ok(())
    );
    assert_eq!(
        fs.owner("/tool"),
        Ok(Owner {
            mode: 0o755,
            uid: MOVER.0,
            gid: MOVER.1
        }),
        "the mover may not give it away, so it is the mover's, set-ID bit dropped"
    );
    let fs = MemFs::new()
        .file("/src/tool", b"bytes")
        .owned("/src/tool", theirs)
        .may_give_away()
        .cross_device("/src");
    assert_eq!(
        run(mv(false, &["/src/tool"], "/tool"), &fs, &Recorder::new()),
        Ok(())
    );
    assert_eq!(fs.owner("/tool"), Ok(theirs));
}

#[test]
fn a_directory_moved_across_volumes_takes_its_mode_after_its_entries() {
    let fs = MemFs::new()
        .dir("/src/ro")
        .file("/src/ro/f", b"x")
        .owned(
            "/src/ro",
            Owner {
                mode: 0o555,
                uid: MOVER.0,
                gid: MOVER.1,
            },
        )
        .cross_device("/src");
    assert_eq!(
        run(mv(false, &["/src/ro"], "/ro"), &fs, &Recorder::new()),
        Ok(())
    );
    assert_eq!(fs.owner("/ro").map(|owner| owner.mode), Ok(0o555));
    assert_eq!(fs.contents("/ro/f").as_deref(), Some(&b"x"[..]));
}

#[test]
fn no_clobber_keeps_a_destination_that_appears_while_a_move_across_volumes_runs() {
    let fs = MemFs::new()
        .file("/src/a", b"new")
        .appears_at_rename("/dst", b"theirs")
        .cross_device("/src");
    let staged = fs.staging_for("/dst");
    assert_eq!(
        run(mv(true, &["/src/a"], "/dst"), &fs, &Recorder::new()),
        Ok(())
    );
    assert_eq!(fs.contents("/dst").as_deref(), Some(&b"theirs"[..]));
    assert_eq!(fs.contents("/src/a").as_deref(), Some(&b"new"[..]));
    assert!(!fs.exists(&staged));
}

#[test]
fn verbose_reports_a_move_across_volumes_as_a_copy_and_a_removal_and_nothing_it_skipped() {
    let fs = MemFs::new()
        .file("/src/a", b"x")
        .file("/kept", b"k")
        .file("/b", b"y")
        .cross_device("/src");
    let out = Recorder::new();
    let verbose = |clobber| Options {
        verbose: true,
        clobber,
        ..Options::DEFAULT
    };
    assert_eq!(
        run(
            mv_with(verbose(Clobber::Overwrite), &["/src/a"], "/dst"),
            &fs,
            &out
        ),
        Ok(())
    );
    assert_eq!(
        run(mv_with(verbose(Clobber::Skip), &["/b"], "/kept"), &fs, &out),
        Ok(())
    );
    assert_eq!(out.text(), "copied '/src/a' -> '/dst'\nremoved '/src/a'\n");
}

#[test]
fn a_name_holding_control_bytes_is_quoted_in_every_message() {
    let refused = MvError::Stat {
        path: String::from("/a\nb\u{1b}[2J"),
        errno: Errno::NotFound,
    };
    assert_eq!(
        refused.to_string(),
        format!(
            "cannot stat '/a'$'\\n''b'$'\\033''[2J': {}",
            Errno::NotFound
        )
    );
    let moved = Recorder::new();
    let fs = MemFs::new().file("/a\u{1b}", b"x");
    let verbose = Options {
        verbose: true,
        ..Options::DEFAULT
    };
    assert_eq!(
        run(mv_with(verbose, &["/a\u{1b}"], "/b"), &fs, &moved),
        Ok(())
    );
    assert_eq!(moved.text(), "renamed '/a'$'\\033' -> '/b'\n");
}
