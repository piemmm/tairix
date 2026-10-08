//! The production, VFS-backed [`DirectorySource`] engine.
//!
//! [`VfsDirectorySource`] is what the shipping file-manager program wires
//! behind the [`Browser`](crate::Browser): it spells the browser's
//! root-first components into one bounded, validated absolute path, hands
//! that path to an injected *fetch* primitive (on a running system,
//! `tairix_rt::read_dir_all` — the kernel-authorised `fs_open` +
//! `fs_readdir` transfer under the caller's own attested identity), and maps
//! the returned packed `DirEntry` stream onto the browser's [`Entry`]
//! vocabulary through the shared `lib/abi` stream walker.
//!
//! Keeping the fetch injected keeps the whole engine host-provable: tests
//! drive a `Browser` end to end over an in-memory tree of *encoded* streams,
//! so every path-spelling, decode, and refusal branch runs in `cargo test`
//! with no kernel. The engine adds no authority of its own — every
//! permission decision stays kernel-side behind the fetch — and it fails
//! closed: a malformed component, an over-long path, or one bad stream
//! record refuses the whole listing rather than showing a partial or guessed
//! one.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::fs::{DirEntries, DirEntry, FileKind, FS_PATH_MAX};
use tairix_abi::window_ipc::WINDOW_TITLE_MAX;
use tairix_abi::Errno;
use tairix_font::ELLIPSIS;

use crate::entry::{Entry, EntryKind, LinkResolution};
use crate::source::{DirectorySource, Listing, Probe};

/// What a listed symbolic link holds, as the browser must see it.
///
/// A `fs_readdir` stream reports *that* a child is a link and nothing more, so
/// both facts a file manager needs come from here: the target the link stores
/// (which the user is shown) and what that target resolves to (which every
/// verb acts on).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LinkInfo {
    /// The target the link stores, **verbatim** — possibly relative, possibly
    /// naming nothing. It is data, never a path this engine walks.
    pub target: String,
    /// The kind the target resolves to, or [`None`] when the target cannot be
    /// reached at all: a dangling link, or one the caller may not follow.
    pub kind: Option<FileKind>,
}

/// Describes a listed symbolic link — the one thing a directory stream cannot
/// carry.
///
/// Kept a seam for the same reason the directory fetch is: the calls it makes
/// are the caller's own capability-checked ones, and keeping them injected is
/// what lets the whole classification run in `cargo test` with no kernel. It
/// is consulted only for entries the stream already reported as links, so a
/// directory of plain files costs nothing.
pub trait LinkReader {
    /// Describe the symbolic link at the absolute `path`, or [`None`] when
    /// even its stored target could not be read (a refusal, or a mounted
    /// format that stores no links).
    fn describe(&mut self, path: &str) -> Option<LinkInfo>;
}

/// A [`LinkReader`] that describes nothing, so every link classifies as
/// dangling.
///
/// For a caller listing a tree that cannot hold links — a store the build
/// itself authored — and for tests that plant none. It is never the right
/// choice for a user's own directory: a link there would be shown as broken
/// when it is not.
pub struct NoLinks;

impl LinkReader for NoLinks {
    fn describe(&mut self, _path: &str) -> Option<LinkInfo> {
        None
    }
}

impl<F: FnMut(&str) -> Option<LinkInfo>> LinkReader for F {
    fn describe(&mut self, path: &str) -> Option<LinkInfo> {
        self(path)
    }
}

// The listing returned here is in the source's own stream order; the
// `Browser` applies the shared sort. This function only decodes and maps,
// so a caller that wants raw stream order (a test, a `du`-style walk) sees
// exactly what the kernel produced.

/// Spell root-first `components` as an absolute path (an empty slice is the
/// root `/`).
///
/// The one path spelling in this app: the browser's displayed path, the
/// tests' tree keys, and the VFS fetch all go through it, so the three can
/// never disagree about what a component list names.
#[must_use]
pub fn spell_absolute_path(components: &[String]) -> String {
    if components.is_empty() {
        return String::from("/");
    }
    let mut path = String::new();
    for component in components {
        path.push('/');
        path.push_str(component);
    }
    path
}

/// Spell root-first `components` as the location text a window title carries,
/// fitted to `budget` bytes by dropping whole leading components behind a
/// leading [`ELLIPSIS`].
///
/// A window title is a bounded field ([`WINDOW_TITLE_MAX`]) and a path is not,
/// so something has to give. Whole components go, oldest first: the directory
/// the window is *in* is the part the reader needs, and a title cut at the tail
/// would hide exactly that. The mark is the one the text engine elides with, so
/// a title the window manager shortens further reads the same way.
///
/// The result never exceeds `budget` bytes and always breaks on a `char`
/// boundary. A `budget` too small even for the mark and the leaf yields the
/// longest prefix of the leaf that fits — a shortened name rather than nothing.
///
/// A control character in a name — which the kernel refuses to *create* but a
/// foreign volume can still carry — is shown as [`char::REPLACEMENT_CHARACTER`],
/// so the text is always one the bounded title field accepts and a hostile name
/// cannot leave a window permanently unable to state where it is. Only the title
/// is spelled this way: [`spell_absolute_path`] stays byte-exact, because it
/// names a real path to open.
#[must_use]
pub fn spell_title_location(components: &[String], budget: usize) -> String {
    let shown: Option<Vec<String>> = components
        .iter()
        .any(|name| name.chars().any(char::is_control))
        .then(|| components.iter().map(|name| shown_name(name)).collect());
    let components = shown.as_deref().unwrap_or(components);
    let full = spell_absolute_path(components);
    if full.len() <= budget {
        return full;
    }
    for skip in 1..components.len() {
        let tail = spell_absolute_path(&components[skip..]);
        if ELLIPSIS.len().saturating_add(tail.len()) <= budget {
            let mut fitted = String::from(ELLIPSIS);
            fitted.push_str(&tail);
            return fitted;
        }
    }
    let leaf = components.last().map_or("", String::as_str);
    let Some(room) = budget.checked_sub(ELLIPSIS.len().saturating_add(1)) else {
        return String::from(cut_to_bytes(leaf, budget));
    };
    let mut fitted = String::from(ELLIPSIS);
    fitted.push('/');
    fitted.push_str(cut_to_bytes(leaf, room));
    fitted
}

/// What leads a document window's title while the document holds changes.
const MODIFIED_MARK: &str = "*";

/// What follows the name of a document its window may not save over.
const READ_ONLY_MARK: &str = " (read-only)";

/// What parts the document's name from the application's in a title.
const APP_SEPARATOR: &str = " \u{2014} ";

/// Write the title a document window shows over `title`, reusing its room:
/// `name`, marked while it holds unsaved changes and when it may only be
/// read, then `app` — the name cut to what the bounded title field leaves.
pub fn write_document_title(
    title: &mut String,
    name: &str,
    modified: bool,
    read_only: bool,
    app: &str,
) {
    title.clear();
    let modified = if modified { MODIFIED_MARK } else { "" };
    let read_only = if read_only { READ_ONLY_MARK } else { "" };
    let budget = WINDOW_TITLE_MAX
        .saturating_sub(modified.len() + read_only.len())
        .saturating_sub(APP_SEPARATOR.len() + app.len());
    title.push_str(modified);
    push_title_name(title, name, budget);
    title.push_str(read_only);
    title.push_str(APP_SEPARATOR);
    title.push_str(app);
}

/// Append the document name `name` to `title` as a window title carries it,
/// in at most `budget` bytes: every control character shown as
/// [`char::REPLACEMENT_CHARACTER`], and a name too long cut on a `char`
/// boundary before a trailing [`ELLIPSIS`].
///
/// A window title is a bounded field and a name is not, so a long one is
/// shortened rather than left to make the whole title unacceptable; and a
/// control character in it would make any title refused.
pub(crate) fn push_title_name(title: &mut String, name: &str, budget: usize) {
    let whole: usize = name.chars().map(|ch| shown(ch).len_utf8()).sum();
    let room = if whole <= budget {
        budget
    } else {
        budget.saturating_sub(ELLIPSIS.len())
    };
    let mut used = 0;
    for ch in name.chars().map(shown) {
        used += ch.len_utf8();
        if used > room {
            break;
        }
        title.push(ch);
    }
    if whole > budget && budget >= ELLIPSIS.len() {
        title.push_str(ELLIPSIS);
    }
}

/// `ch` as a reader is shown it: a control character as
/// [`char::REPLACEMENT_CHARACTER`], which a display can render, rather than
/// the byte itself.
fn shown(ch: char) -> char {
    if ch.is_control() {
        char::REPLACEMENT_CHARACTER
    } else {
        ch
    }
}

/// `name` with every character shown as a reader is shown it.
fn shown_name(name: &str) -> String {
    name.chars().map(shown).collect()
}

/// The longest prefix of `text` that fits in `budget` bytes, cut on a `char`
/// boundary.
fn cut_to_bytes(text: &str, budget: usize) -> &str {
    if text.len() <= budget {
        return text;
    }
    let mut end = 0;
    for (index, ch) in text.char_indices() {
        let next = index.saturating_add(ch.len_utf8());
        if next > budget {
            break;
        }
        end = next;
    }
    &text[..end]
}

/// Append the leaf `name` to the directory path already spelled in `path`.
///
/// The one place a directory path and a leaf are joined, so the root case
/// (`/` plus a name, never `//name`) is handled once. Appending in place lets
/// a caller that spells many children of one directory — a grid of tiles —
/// reuse a single buffer instead of allocating a path each time.
pub fn push_child(path: &mut String, name: &str) {
    if !path.ends_with('/') {
        path.push('/');
    }
    path.push_str(name);
}

/// Parse an absolute path string into validated root-first components — the
/// inverse of [`spell_absolute_path`].
///
/// This is how a consumer turns a path it was *handed* (the `HOME` environment
/// the desktop session reads to open its picker at the user's home) into the
/// component list the [`Browser`](crate::Browser) navigates, using the *same*
/// per-component rule [`absolute_path`] spells with: every segment between `/`s
/// must be a real single filesystem leaf name
/// ([`tairix_path::validate_file_name`]). Leading, trailing, and repeated `/`s
/// collapse (so `/Users/root/` and `/Users/root` parse alike); the bare root
/// `/` (and the empty string) parse to no components.
///
/// # Errors
///
/// * [`Errno::OutOfRange`] if any segment is not a valid leaf name (`.`,
///   `..`, a control character, `:`, or over the name bound), so a caller
///   falls back to a directory it can name rather than guessing.
/// * [`Errno::LengthOutOfRange`] if the re-spelled path exceeds
///   [`FS_PATH_MAX`].
pub fn components_from_absolute_path(path: &str) -> Result<Vec<String>, Errno> {
    let mut components = Vec::new();
    for segment in path.split('/') {
        if segment.is_empty() {
            continue;
        }
        tairix_path::validate_file_name(segment).map_err(|_| Errno::OutOfRange)?;
        components.push(String::from(segment));
    }
    if spell_absolute_path(&components).len() > FS_PATH_MAX {
        return Err(Errno::LengthOutOfRange);
    }
    Ok(components)
}

/// Validate `components` and spell them as the absolute path handed to the
/// kernel.
///
/// Every component must be a real single filesystem leaf name — the shared
/// [`tairix_path::validate_file_name`] rule (non-empty, not `.` or `..`, no
/// `/`, no control character or NUL, no `:`, within the name bound) — so a
/// component can never name a different directory than the browser shows and
/// the check is the *same* one the rename editor spells a new name through. The
/// spelled path is bounded by the kernel's own [`FS_PATH_MAX`].
///
/// # Errors
///
/// * [`Errno::OutOfRange`] for a malformed component.
/// * [`Errno::LengthOutOfRange`] for a path longer than [`FS_PATH_MAX`].
pub fn absolute_path(components: &[String]) -> Result<String, Errno> {
    for component in components {
        tairix_path::validate_file_name(component).map_err(|_| Errno::OutOfRange)?;
    }
    let path = spell_absolute_path(components);
    if path.len() > FS_PATH_MAX {
        return Err(Errno::LengthOutOfRange);
    }
    Ok(path)
}

/// Map one packed `fs_readdir` stream onto the browser's [`Entry`] list.
///
/// The walk is the shared [`DirEntries`] iterator; the first malformed
/// record (or a non-UTF-8 name, refused with the decoder's own domain
/// errno) refuses the whole listing — the browser never shows a partial
/// directory as complete.
///
/// # Errors
///
/// Any [`Errno`] the stream walker surfaces, or [`Errno::OutOfRange`] for a
/// name that is not UTF-8.
pub fn entries_from_dir_stream(
    directory: &str,
    stream: &[u8],
    links: &mut dyn LinkReader,
) -> Result<Vec<Entry>, Errno> {
    let mut entries = Vec::new();
    for item in DirEntries::new(stream) {
        entries.push(entry_from_record(directory, &item?, links)?);
    }
    Ok(entries)
}

/// The [`Entry`] one listing record of `directory` stands for — the one
/// decode a listing and a watch's changed records share.
///
/// # Errors
///
/// [`Errno::OutOfRange`] for a name that is not UTF-8.
pub(crate) fn entry_from_record(
    directory: &str,
    entry: &DirEntry<'_>,
    links: &mut dyn LinkReader,
) -> Result<Entry, Errno> {
    let name = core::str::from_utf8(entry.name).map_err(|_| Errno::OutOfRange)?;
    // Only a link costs a second call: the stream already carries every
    // other kind outright, and carries neither a link's target nor what
    // that target is.
    let described = match entry.kind {
        FileKind::Symlink => links.describe(&tairix_path::join(directory, name)),
        FileKind::Directory | FileKind::Regular => None,
    };
    let resolution = described.as_ref().and_then(|link| {
        link.kind.map(|kind| LinkResolution {
            kind,
            target_name: tairix_path::leaf_name(&link.target),
        })
    });
    let kind = EntryKind::for_listing(entry.kind, name, resolution);
    let mut built = Entry::new(name, kind, entry.size, entry.modified).with_id(entry.id);
    // A link whose target could not be reached still shows the spelling it
    // stores: that spelling is exactly what tells a user why it is broken.
    if let Some(link) = described {
        built = built.with_target(link.target);
    }
    Ok(built)
}

/// The production [`DirectorySource`]: validated path spelling composed
/// with an injected directory fetch and the shared stream decode.
///
/// `fetch` receives the spelled absolute path and returns the raw packed
/// `DirEntry` stream for that directory; the shipping program passes the
/// `lib/rt` listing call, tests pass an in-memory tree. The fetch carries
/// the only authority involved — the engine itself makes no permission
/// decision and fabricates no entry.
///
/// A source built with [`new`](VfsDirectorySource::new) offers no occupancy
/// probe: [`has_children`](DirectorySource::has_children) answers
/// [`Errno::NotImplemented`], the browser records the child as
/// [`Occupancy::Indeterminate`](crate::Occupancy::Indeterminate), and it draws
/// the plain folder. That is the trusted file picker's deliberate choice — a
/// read-only chooser gains nothing from the cue, so it reads no directory it
/// is not showing. [`probing`](VfsDirectorySource::probing) opts in.
pub struct VfsDirectorySource<F, L, P = NoProbe> {
    fetch: F,
    links: L,
    probe: Option<P>,
}

/// The probe type of a source that does not probe — the default `P`, so
/// [`VfsDirectorySource::new`] infers a concrete type without a caller naming
/// one.
pub type NoProbe = fn(&str, &mut [u8]) -> Result<usize, Errno>;

/// Bytes handed to a probe: one batch holding a folder's first several dozen
/// entries, which its picture is sampled from. An empty batch is an empty
/// folder.
pub const PROBE_BUF_LEN: usize = 4096;

/// Probe each folder in `batch` through `probe`, as a
/// [`probing`](VfsDirectorySource::probing) source does, answering for every
/// one — a refusal included, so the folder draws plain and is not asked again.
///
/// The read a worker performs for a [`Probes`](crate::Probes) batch. One
/// [`PROBE_BUF_LEN`] buffer serves the whole batch.
pub fn probe_batch<P>(
    batch: Vec<Vec<String>>,
    mut probe: P,
) -> Vec<(Vec<String>, Result<Probe, Errno>)>
where
    P: FnMut(&str, &mut [u8]) -> Result<usize, Errno>,
{
    let mut buf = [0u8; PROBE_BUF_LEN];
    batch
        .into_iter()
        .map(|folder| {
            let answer = probe_folder(&folder, &mut probe, &mut buf);
            (folder, answer)
        })
        .collect()
}

/// What `probe` reads of the folder named by `components`, into `buf`.
fn probe_folder<P>(components: &[String], probe: &mut P, buf: &mut [u8]) -> Result<Probe, Errno>
where
    P: FnMut(&str, &mut [u8]) -> Result<usize, Errno>,
{
    let path = absolute_path(components)?;
    let read = probe(&path, buf)?;
    probed(components, buf.get(..read).ok_or(Errno::OutOfRange)?)
}

/// What one batch of the entries of the folder named by `components`,
/// `stream`, says of it: that it is empty, or what it holds.
///
/// A batch that does not decode refuses the probe, as it would refuse the
/// listing.
fn probed(components: &[String], stream: &[u8]) -> Result<Probe, Errno> {
    for record in DirEntries::new(stream) {
        record?;
    }
    if stream.is_empty() {
        return Ok(Probe::Empty);
    }
    let named = DirEntries::new(stream)
        .flatten()
        .filter_map(|record| Some((core::str::from_utf8(record.name).ok()?, record.kind)));
    Ok(Probe::Holds(crate::folder_sample(components, named)))
}

impl<F, L> VfsDirectorySource<F, L>
where
    F: FnMut(&str) -> Result<Vec<u8>, Errno>,
    L: LinkReader,
{
    /// Build the source over `fetch`, the capability-checked directory read,
    /// and `links`, which describes the symbolic links the listing reports.
    pub fn new(fetch: F, links: L) -> Self {
        Self {
            fetch,
            links,
            probe: None,
        }
    }
}

impl<F, L, P> VfsDirectorySource<F, L, P>
where
    F: FnMut(&str) -> Result<Vec<u8>, Errno>,
    L: LinkReader,
    P: FnMut(&str, &mut [u8]) -> Result<usize, Errno>,
{
    /// Build the source over `fetch` and an occupancy `probe`.
    ///
    /// `probe` opens the directory named by the spelled path, reads one batch
    /// into the given buffer, and closes it, returning the bytes the batch
    /// occupies — on a running system `tairix_rt::open_dir` plus one
    /// `Dir::read`. A batch costs the kernel what the buffer holds, so the
    /// [`PROBE_BUF_LEN`] buffer it is handed costs the same on a directory of
    /// a hundred entries and on one of a hundred thousand.
    pub fn probing(fetch: F, links: L, probe: P) -> Self {
        Self {
            fetch,
            links,
            probe: Some(probe),
        }
    }
}

impl<F, L, P> DirectorySource for VfsDirectorySource<F, L, P>
where
    F: FnMut(&str) -> Result<Vec<u8>, Errno>,
    L: LinkReader,
    P: FnMut(&str, &mut [u8]) -> Result<usize, Errno>,
{
    fn list(&mut self, components: &[String]) -> Result<Listing, Errno> {
        let path = absolute_path(components)?;
        let stream = (self.fetch)(&path)?;
        // The fetch is this thread's own read, so the answer is always ready.
        entries_from_dir_stream(&path, &stream, &mut self.links).map(Listing::Ready)
    }

    fn has_children(&mut self, components: &[String]) -> Result<Probe, Errno> {
        let probe = self.probe.as_mut().ok_or(Errno::NotImplemented)?;
        // This thread's own read, so the answer is always ready.
        probe_folder(components, probe, &mut [0u8; PROBE_BUF_LEN])
    }
}

/// The production [`LinkReader`]: the kernel-authorised `fs_readlink` plus one
/// resolving `fs_stat`.
///
/// It adds no authority — both calls are the caller's own, authorised per
/// inode under its attested identity — and it fails closed: a link whose
/// target cannot be read is described as nothing at all, and one whose target
/// cannot be *reached* is described with no kind, which classifies it as
/// dangling rather than guessing.
///
/// One implementation, shared by every surface that lists a directory the user
/// can put a link in (the file manager, the desktop, the trusted picker, the
/// wallpaper catalog), so the two calls are never re-derived.
#[cfg(feature = "rt")]
pub struct RtLinkReader;

/// The production probe: open the directory at `path` under the caller's own
/// identity and read one batch into `buf`, answering the bytes it holds. A
/// batch costs what its buffer holds, however large the directory, and a
/// directory the caller may not read simply refuses.
///
/// # Errors
///
/// The kernel's refusal of the open or the read.
#[cfg(feature = "rt")]
pub fn probe_directory(path: &str, buf: &mut [u8]) -> Result<usize, Errno> {
    let dir = tairix_rt::open_dir(path.as_bytes()).map_err(Errno::from_syscall)?;
    dir.read(buf).map_err(Errno::from_syscall)
}

#[cfg(feature = "rt")]
impl LinkReader for RtLinkReader {
    fn describe(&mut self, path: &str) -> Option<LinkInfo> {
        // The target is bounded by `FS_SYMLINK_MAX`, and `fs_readlink`
        // refuses an undersized buffer rather than truncating a target — a
        // truncated one would name somewhere else entirely — so one buffer of
        // that size always suffices.
        let mut buf = alloc::vec![0u8; tairix_abi::fs::FS_SYMLINK_MAX];
        let ret = tairix_rt::fs_readlink(path.as_bytes(), &mut buf);
        let len = usize::try_from(ret).ok()?;
        let target = core::str::from_utf8(buf.get(..len)?).ok()?;
        // A resolve-only open with the *following* posture: no read authority
        // is requested, the handle closes on drop, and a target that cannot be
        // reached simply yields no kind.
        let kind = tairix_rt::File::open(path.as_bytes(), tairix_abi::fs::OpenFlags::empty())
            .ok()
            .and_then(|file| file.stat().ok())
            .map(|stat| stat.kind);
        Some(LinkInfo {
            target: String::from(target),
            kind,
        })
    }
}
