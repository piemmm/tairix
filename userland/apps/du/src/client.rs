//! The `du` engine: walk each operand, sum usage, and print the rows.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::{Errno, FileId, FileKind};
use tairix_collections::HashSet;
use tairix_hash::BuildSipHash13;
use tairix_help::{own_short_help, HelpSource};
use tairix_path::join;
use tairix_util::size::{blocks_ceil, format_human, format_u128, SizeScale, SIZE_TEXT_MAX};

use crate::command::{Command, Dereference, Options};
use crate::error::DuError;
use crate::io::{Entry, FinalLink, Metadata, Output, Walk};
use tairix_util::quote::{output_style, quote, Quoted, Style};

/// The one-line usage banner, the short help's fallback when the bundled
/// help document is unavailable.
pub const USAGE: &str =
    "usage: du [-a | -s] [-clS0] [-H | -L | -P] [-h | -k | -m | -b | --si | -B <size>] [--apparent-size] [-d <n>] [--] [file...]";

/// The command word this bundle is named by, for the own-help lookup.
const OWN_WORD: &str = "du";

/// Run a parsed `du` command against the injected seams. `keys` is the
/// process's hash key, which the identities of nodes already counted are
/// kept under, since a hostile volume chooses them.
///
/// Returns `Ok(true)` when every operand was walked cleanly, `Ok(false)`
/// when at least one path was diagnosed on standard error (the GNU
/// behaviour: report, continue, exit `1`).
///
/// # Errors
///
/// [`DuError::Output`] when a row (or the short help) cannot be written.
pub fn run(
    command: Command,
    locale: Option<&str>,
    walk: &dyn Walk,
    help: &dyn HelpSource,
    out: &dyn Output,
    err: &dyn Output,
    keys: BuildSipHash13,
) -> Result<bool, DuError> {
    let options = match command {
        Command::Help => {
            let bytes = own_short_help(help, locale, OWN_WORD)
                .unwrap_or_else(|| format!("{USAGE}\n").into_bytes());
            out.write_all(&bytes).map_err(DuError::Output)?;
            return Ok(true);
        }
        Command::Report(options) => options,
    };
    let mut report = Reporter {
        options: &options,
        walk,
        out,
        err,
        names: output_style(out.at_terminal() && !options.null_terminated),
        clean: true,
        grand_total: 0,
        counted: Counted {
            seen: HashSet::with_hasher(keys),
        },
        warned: false,
    };
    if options.paths.is_empty() {
        report.operand(".")?;
    } else {
        for path in &options.paths {
            report.operand(path)?;
        }
    }
    if options.grand_total {
        let total = report.grand_total;
        report.row(total, "total")?;
    }
    Ok(report.clean)
}

/// The nodes this walk has already counted, so a second sighting — another
/// name for a file, a repeated or nested operand, a node `-L` reaches twice —
/// is left out, as GNU leaves it.
///
/// Only what can be sighted twice is remembered (see [`Reporter::tracked`]):
/// a node named once, met in a single operand's tree without following
/// links, cannot be, which keeps the set to the hard links the walk meets on
/// a volume far larger than memory. The set grows on demand from no fixed
/// ceiling; a heap that refuses to grow it degrades to counting every name,
/// and the run says so on standard error and exits non-zero, so an
/// over-count is never silent.
struct Counted {
    seen: HashSet<FileId, BuildSipHash13>,
}

/// What a node's identity says about whether to count it.
enum Sighting {
    /// The first sighting: count it.
    First,
    /// A node already counted: it is left out.
    Repeat,
    /// Count it, but the node could not be remembered, so a later sighting
    /// will count it again — the caller reports the over-count.
    Untracked,
}

impl Counted {
    /// Record a sighting of the node `id`.
    fn sight(&mut self, id: FileId) -> Sighting {
        // A backing that offers no identity cannot be keyed on one. Counting
        // it risks a double count; merging it with the next such node would
        // silently *lose* real storage, so it is counted and reported.
        if id.is_none() {
            return Sighting::Untracked;
        }
        match self.seen.try_insert(id) {
            Ok(true) => Sighting::First,
            Ok(false) => Sighting::Repeat,
            Err(_) => Sighting::Untracked,
        }
    }
}

/// One in-flight directory of the iterative post-order walk.
struct Frame {
    /// The directory's full path, as reported.
    path: String,
    /// Its identity, so `-L` never re-enters a directory it is inside.
    id: FileId,
    /// Its entries, consumed front to back.
    entries: Vec<Entry>,
    /// Index of the next entry to visit.
    next: usize,
    /// Levels below the operand (the operand itself is depth `0`).
    depth: u64,
    /// The directory's own bytes plus its plain files' bytes.
    own_bytes: u128,
    /// `own_bytes` plus every subdirectory's tree total.
    tree_bytes: u128,
}

/// The walk-and-report state for one `du` run.
struct Reporter<'a> {
    options: &'a Options,
    walk: &'a dyn Walk,
    out: &'a dyn Output,
    err: &'a dyn Output,
    /// How a row shows its path ([`output_style`]); a NUL-ended row is for a
    /// program, which wants the path's own bytes.
    names: Style,
    clean: bool,
    grand_total: u128,
    /// The names already counted, so a hard-linked file is summed once.
    counted: Counted,
    /// Whether the untracked-node diagnostic has already been written, so
    /// it is reported once per run rather than once per entry.
    warned: bool,
}

impl Reporter<'_> {
    /// Walk one operand and report its rows.
    fn operand(&mut self, path: &str) -> Result<(), DuError> {
        let link = match self.options.dereference {
            Dereference::Never => FinalLink::Keep,
            Dereference::Operands | Dereference::Always => FinalLink::Follow,
        };
        let meta = match self.walk.stat(path, link) {
            Ok(meta) => meta,
            Err(errno) => return self.diagnose(path, errno),
        };
        if !self.first_sighting(&meta)? {
            return Ok(());
        }
        if meta.kind != FileKind::Directory {
            let bytes = self.measure(&meta);
            self.grand_total += bytes;
            return self.row(bytes, Quoted::new(path, self.names));
        }
        self.directory(path, &meta)
    }

    /// Walk a directory operand iteratively (an explicit frame stack, so
    /// a deep tree can never exhaust the call stack), reporting each
    /// subdirectory post-order exactly as GNU `du` does.
    fn directory(&mut self, path: &str, meta: &Metadata) -> Result<(), DuError> {
        let mut stack = Vec::new();
        let root = self.open_frame(path, meta, 0)?;
        let Some(root) = root else {
            return Ok(());
        };
        stack.push(root);
        loop {
            // Take the top frame's next entry, releasing the frame borrow
            // before any diagnostics or recursion bookkeeping.
            let next_child = match stack.last_mut() {
                None => break,
                Some(frame) => match frame.entries.get(frame.next).cloned() {
                    Some(entry) => {
                        frame.next += 1;
                        Some((join(&frame.path, &entry.name), entry.meta, frame.depth + 1))
                    }
                    None => None,
                },
            };
            let Some((child_path, child_meta, child_depth)) = next_child else {
                // The top frame is exhausted: report it post-order and fold
                // its tree total into its parent (or the grand total).
                let Some(frame) = stack.pop() else { break };
                let reported = if self.options.separate_dirs {
                    frame.own_bytes
                } else {
                    frame.tree_bytes
                };
                if self.within_depth(frame.depth) {
                    self.row(reported, Quoted::new(&frame.path, self.names))?;
                }
                match stack.last_mut() {
                    Some(parent) => parent.tree_bytes += frame.tree_bytes,
                    None => self.grand_total += frame.tree_bytes,
                }
                continue;
            };
            // The child's metadata came with its directory entry: the one
            // listing already reported it, so the walk never re-resolves a
            // child by path (each such stat is a fresh full walk on an
            // uncached, authenticated volume) — except to follow a link
            // under `-L`.
            let child_meta = if child_meta.kind == FileKind::Symlink
                && self.options.dereference == Dereference::Always
            {
                match self.walk.stat(&child_path, FinalLink::Follow) {
                    Ok(meta) => meta,
                    Err(errno) => {
                        self.unreachable_link(&child_path, errno)?;
                        continue;
                    }
                }
            } else {
                child_meta
            };
            if !self.first_sighting(&child_meta)? {
                continue;
            }
            if child_meta.kind == FileKind::Directory {
                // `-l` keeps no seen-set, so a link back up is caught here.
                let inside =
                    !child_meta.id.is_none() && stack.iter().any(|frame| frame.id == child_meta.id);
                if inside {
                    continue;
                }
                if let Some(child) = self.open_frame(&child_path, &child_meta, child_depth)? {
                    stack.push(child);
                }
                continue;
            }
            let bytes = self.measure(&child_meta);
            if let Some(frame) = stack.last_mut() {
                frame.own_bytes += bytes;
                frame.tree_bytes += bytes;
            }
            if self.options.all && self.within_depth(child_depth) {
                self.row(bytes, Quoted::new(&child_path, self.names))?;
            }
        }
        Ok(())
    }

    /// Open a directory as a walk frame; an unreadable directory is
    /// diagnosed and skipped (its own bytes are not reported — nothing
    /// below it could be counted, so a partial number would be a guess).
    fn open_frame(
        &mut self,
        path: &str,
        meta: &Metadata,
        depth: u64,
    ) -> Result<Option<Frame>, DuError> {
        let entries = match self.walk.read_dir(path) {
            Ok(entries) => entries,
            Err(errno) => {
                self.diagnose(path, errno)?;
                return Ok(None);
            }
        };
        let own = self.measure(meta);
        Ok(Some(Frame {
            path: String::from(path),
            id: meta.id,
            entries,
            next: 0,
            depth,
            own_bytes: own,
            tree_bytes: own,
        }))
    }

    /// Whether a node can be sighted twice, and so is remembered: every node
    /// under `-L` or when operands may repeat or nest; otherwise only a file
    /// with more than one name, since one operand's tree reaches every other
    /// node once. `-l` counts every sighting, so it remembers nothing.
    fn tracked(&self, meta: &Metadata) -> bool {
        if self.options.count_links {
            return false;
        }
        self.options.dereference == Dereference::Always
            || self.options.paths.len() > 1
            || (meta.nlink > 1 && meta.kind != FileKind::Directory)
    }

    /// Whether this is the first sighting of the node `meta`, which is then
    /// counted; a later one is left out.
    fn first_sighting(&mut self, meta: &Metadata) -> Result<bool, DuError> {
        if !self.tracked(meta) {
            return Ok(true);
        }
        match self.counted.sight(meta.id) {
            Sighting::First => Ok(true),
            Sighting::Repeat => Ok(false),
            Sighting::Untracked => self.warn_untracked().map(|()| true),
        }
    }

    /// Report a link `-L` could not follow: GNU names a dangling one alone,
    /// and any other refusal with its reason.
    fn unreachable_link(&mut self, path: &str, errno: Errno) -> Result<(), DuError> {
        if errno != Errno::NotFound {
            return self.diagnose(path, errno);
        }
        self.clean = false;
        let line = format!("du: cannot access {}\n", quote(path));
        self.err.write_all(line.as_bytes()).map_err(DuError::Output)
    }

    /// Report, once, that the walk can no longer tell a repeated name from
    /// a second file, so the totals count some storage more than once.
    fn warn_untracked(&mut self) -> Result<(), DuError> {
        self.clean = false;
        if self.warned {
            return Ok(());
        }
        self.warned = true;
        self.err
            .write_all(
                b"du: cannot track every hard link; multiply-named files are counted once per name\n",
            )
            .map_err(DuError::Output)
    }

    /// The bytes a node contributes under the selected measure.
    fn measure(&self, meta: &Metadata) -> u128 {
        if self.options.apparent_size {
            u128::from(meta.size)
        } else {
            u128::from(meta.allocated)
        }
    }

    /// Whether a node this many levels below its operand is reported.
    fn within_depth(&self, depth: u64) -> bool {
        match self.options.max_depth {
            Some(max_depth) => depth <= max_depth,
            None => true,
        }
    }

    /// Write one `size<TAB>path` row in the selected scale.
    fn row(&self, bytes: u128, label: impl core::fmt::Display) -> Result<(), DuError> {
        let mut buf = [0u8; SIZE_TEXT_MAX];
        let size = match self.options.scale {
            SizeScale::Blocks(unit) => {
                // The unit came from the parser, which refuses zero.
                let blocks = blocks_ceil(bytes, unit).unwrap_or(0);
                format_u128(blocks, &mut buf)
            }
            SizeScale::HumanBinary => format_human(bytes, 1024, &mut buf),
            SizeScale::HumanDecimal => format_human(bytes, 1000, &mut buf),
        };
        let terminator = if self.options.null_terminated {
            '\0'
        } else {
            '\n'
        };
        let line = format!("{size}\t{label}{terminator}");
        self.out.write_all(line.as_bytes()).map_err(DuError::Output)
    }

    /// Report an unreachable path on standard error and mark the run
    /// unclean; the walk continues (GNU behaviour), but a diagnostics
    /// stream that itself fails is fatal.
    fn diagnose(&mut self, path: &str, errno: Errno) -> Result<(), DuError> {
        self.clean = false;
        let line = format!("du: cannot access {path}: {errno}\n", path = quote(path));
        self.err.write_all(line.as_bytes()).map_err(DuError::Output)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::{run, USAGE};
    use crate::command::{parse, Command};
    use crate::error::DuError;
    use crate::io::{Entry, FinalLink, Metadata, Output, Walk};
    use alloc::collections::BTreeMap;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;
    use core::cell::RefCell;
    use tairix_abi::{Errno, FileId, FileKind};
    use tairix_hash::{BuildSipHash13, HashSeed};
    use tairix_help::{HelpSource, SourceError};

    fn keys() -> BuildSipHash13 {
        BuildSipHash13::with_seed(HashSeed::UNKEYED)
    }

    /// One node of the in-memory tree fixture.
    enum Node {
        /// A regular file: `(apparent size, allocated bytes)`.
        File(u64, u64),
        /// A symbolic link naming this path.
        Symlink(String),
        /// A second name for the file at this path — a hard link, so both
        /// names report the one identity and the count of names.
        Link(String),
        /// A file whose backing offers no identity, as a mount point listed
        /// through its parent does: `(size, allocated, name count)`.
        Unidentified(u64, u64, u32),
        /// A directory: `(allocated bytes, entry names)`.
        Dir(u64, Vec<String>),
        /// A path whose stat is refused with this errno.
        Denied(Errno),
        /// A directory that stats fine but refuses to list.
        Unlistable(u64),
    }

    /// An in-memory `Walk` over a fixed path → node map.
    struct MemFs {
        nodes: BTreeMap<String, Node>,
    }

    impl MemFs {
        fn new(nodes: &[(&str, Node)]) -> Self {
            Self {
                nodes: nodes
                    .iter()
                    .map(|(path, node)| {
                        (
                            (*path).to_string(),
                            match node {
                                Node::File(size, allocated) => Node::File(*size, *allocated),
                                Node::Link(target) => Node::Link(target.clone()),
                                Node::Symlink(target) => Node::Symlink(target.clone()),
                                Node::Unidentified(size, allocated, names) => {
                                    Node::Unidentified(*size, *allocated, *names)
                                }
                                Node::Dir(own, names) => Node::Dir(*own, names.clone()),
                                Node::Denied(errno) => Node::Denied(*errno),
                                Node::Unlistable(own) => Node::Unlistable(*own),
                            },
                        )
                    })
                    .collect(),
            }
        }
    }

    impl MemFs {
        /// The identity and name count of the file object at `object`: a
        /// distinct node number per path that holds a file, plus one name
        /// for every hard link naming it.
        fn identity(&self, object: &str) -> (FileId, u32) {
            let index = self
                .nodes
                .keys()
                .position(|key| key == object)
                .expect("the object is in the fixture");
            let links = self
                .nodes
                .values()
                .filter(|node| matches!(node, Node::Link(target) if target == object))
                .count();
            (
                FileId {
                    volume: [1u8; 16],
                    // A distinct, non-zero number per object, so no real
                    // node is ever mistaken for `FileId::NONE`.
                    node: u64::try_from(index).expect("fixture index") + 1,
                },
                u32::try_from(links).expect("fixture links") + 1,
            )
        }
    }

    impl MemFs {
        /// `path` with every link on the way resolved, and its final one too
        /// under [`FinalLink::Follow`], as a walk through a followed link
        /// reaches it. Bounded, so a cycle cannot spin.
        fn resolve(&self, path: &str, link: FinalLink) -> Result<String, Errno> {
            let mut hops = 0;
            let mut at = String::new();
            let parts: Vec<&str> = path.split('/').collect();
            for (index, part) in parts.iter().enumerate() {
                at = if at.is_empty() {
                    (*part).to_string()
                } else {
                    alloc::format!("{at}/{part}")
                };
                let last = index + 1 == parts.len();
                while let Some(Node::Symlink(target)) = self.nodes.get(&at) {
                    if last && link == FinalLink::Keep {
                        break;
                    }
                    hops += 1;
                    if hops > 8 {
                        return Err(Errno::LinkLoop);
                    }
                    at = target.clone();
                }
            }
            Ok(at)
        }
    }

    impl Walk for MemFs {
        fn stat(&self, path: &str, link: FinalLink) -> Result<Metadata, Errno> {
            let path = self.resolve(path, link)?;
            let path = path.as_str();
            match self.nodes.get(path) {
                Some(Node::Symlink(target)) => Ok(Metadata {
                    kind: FileKind::Symlink,
                    size: u64::try_from(target.len()).expect("short target"),
                    allocated: 0,
                    id: self.identity(path).0,
                    nlink: 1,
                }),
                Some(Node::File(size, allocated)) => {
                    let (id, nlink) = self.identity(path);
                    Ok(Metadata {
                        kind: FileKind::Regular,
                        size: *size,
                        allocated: *allocated,
                        id,
                        nlink,
                    })
                }
                Some(Node::Link(target)) => {
                    let Some(Node::File(size, allocated)) = self.nodes.get(target) else {
                        return Err(Errno::NotFound);
                    };
                    let (id, nlink) = self.identity(target);
                    Ok(Metadata {
                        kind: FileKind::Regular,
                        size: *size,
                        allocated: *allocated,
                        id,
                        nlink,
                    })
                }
                Some(Node::Unidentified(size, allocated, nlink)) => Ok(Metadata {
                    kind: FileKind::Regular,
                    size: *size,
                    allocated: *allocated,
                    id: FileId::NONE,
                    nlink: *nlink,
                }),
                Some(Node::Dir(own, _) | Node::Unlistable(own)) => Ok(Metadata {
                    kind: FileKind::Directory,
                    size: 0,
                    allocated: *own,
                    id: self.identity(path).0,
                    // A directory's count includes its own `.` and each
                    // child's `..`, so it always exceeds one.
                    nlink: 2,
                }),
                Some(Node::Denied(errno)) => Err(*errno),
                None => Err(Errno::NotFound),
            }
        }

        fn read_dir(&self, path: &str) -> Result<Vec<Entry>, Errno> {
            let path = self.resolve(path, FinalLink::Follow)?;
            let path = path.as_str();
            match self.nodes.get(path) {
                Some(Node::Dir(_, names)) => names
                    .iter()
                    .map(|name| {
                        let child = if path.ends_with('/') {
                            alloc::format!("{path}{name}")
                        } else {
                            alloc::format!("{path}/{name}")
                        };
                        // The listing reports each child's own metadata, as
                        // the production `fs_readdir` stream does.
                        let meta = self.stat(&child, FinalLink::Keep)?;
                        Ok(Entry {
                            name: name.clone(),
                            meta,
                        })
                    })
                    .collect(),
                Some(Node::Unlistable(_)) => Err(Errno::PermissionDenied),
                _ => Err(Errno::NotFound),
            }
        }
    }

    /// Captures everything written to one stream.
    #[derive(Default)]
    struct MemOut {
        bytes: RefCell<Vec<u8>>,
    }

    impl MemOut {
        fn text(&self) -> String {
            String::from_utf8(self.bytes.borrow().clone()).expect("utf-8 output")
        }
    }

    impl Output for MemOut {
        fn write_all(&self, bytes: &[u8]) -> Result<(), Errno> {
            self.bytes.borrow_mut().extend_from_slice(bytes);
            Ok(())
        }
    }

    /// A stream that refuses every write.
    struct BrokenSink;

    impl Output for BrokenSink {
        fn write_all(&self, _bytes: &[u8]) -> Result<(), Errno> {
            Err(Errno::NotImplemented)
        }
    }

    /// A help tree with no documents, so the usage banner is the fallback.
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

    /// A three-level tree: 512-byte directory metadata, two files under
    /// the root, one file in a subdirectory.
    fn tree() -> MemFs {
        MemFs::new(&[
            (
                "docs",
                Node::Dir(512, alloc::vec!["a".to_string(), "sub".to_string()]),
            ),
            ("docs/a", Node::File(100, 1024)),
            ("docs/sub", Node::Dir(512, alloc::vec!["b".to_string()])),
            ("docs/sub/b", Node::File(3000, 4096)),
        ])
    }

    /// A tree whose one 4096-byte file is reached through two names, plus a
    /// second, singly-named file so the deduplication is visibly selective.
    fn linked_tree() -> MemFs {
        MemFs::new(&[
            (
                "docs",
                Node::Dir(
                    512,
                    alloc::vec!["one".to_string(), "other".to_string(), "two".to_string()],
                ),
            ),
            ("docs/one", Node::File(3000, 4096)),
            ("docs/other", Node::File(100, 1024)),
            ("docs/two", Node::Link("docs/one".to_string())),
        ])
    }

    /// A capturing stream the kernel attests as a terminal.
    #[derive(Default)]
    struct Console(MemOut);

    impl Output for Console {
        fn write_all(&self, bytes: &[u8]) -> Result<(), Errno> {
            self.0.write_all(bytes)
        }

        fn at_terminal(&self) -> bool {
            true
        }
    }

    #[test]
    fn a_label_is_escaped_at_a_terminal_and_kept_in_a_pipe() {
        let fs = MemFs::new(&[("e\u{1b}[2Jx", Node::File(100, 1024))]);
        let out = Console::default();
        let command = parse(&["-c", "e\u{1b}[2Jx"]).expect("parse");
        run(
            command,
            None,
            &fs,
            &NoHelp,
            &out,
            &MemOut::default(),
            keys(),
        )
        .expect("run");
        assert_eq!(out.0.text(), "1\t'e'$'\\033''[2Jx'\n1\ttotal\n");
        let zero = Console::default();
        let command = parse(&["-0", "e\u{1b}[2Jx"]).expect("parse");
        run(
            command,
            None,
            &fs,
            &NoHelp,
            &zero,
            &MemOut::default(),
            keys(),
        )
        .expect("run");
        assert_eq!(zero.0.text(), "1\te\u{1b}[2Jx\0");
        assert_eq!(run_case(&["e\u{1b}[2Jx"], &fs).1, "1\te\u{1b}[2Jx\n");
    }

    fn run_case(args: &[&str], fs: &MemFs) -> (bool, String, String) {
        let command = parse(args).expect("parse");
        let out = MemOut::default();
        let err = MemOut::default();
        let clean = run(command, None, fs, &NoHelp, &out, &err, keys()).expect("run");
        (clean, out.text(), err.text())
    }

    #[test]
    fn reports_directories_post_order_in_kibibytes() {
        // sub: 512 + 4096 = 4608 B → 5 blocks; docs: 512 + 1024 + 4608 =
        // 6144 B → 6 blocks. Post-order: the subdirectory first.
        let (clean, out, err) = run_case(&["docs"], &tree());
        assert!(clean);
        assert_eq!(out, "5\tdocs/sub\n6\tdocs\n");
        assert!(err.is_empty());
    }

    #[test]
    fn all_reports_files_and_null_terminates() {
        let (clean, out, _) = run_case(&["-a0", "docs"], &tree());
        assert!(clean);
        // docs/a: 1024 B → 1 block; docs/sub/b: 4096 B → 4 blocks.
        assert_eq!(
            out,
            "1\tdocs/a\x004\tdocs/sub/b\x005\tdocs/sub\x006\tdocs\x00"
        );
    }

    #[test]
    fn summarize_and_total_report_the_operand_totals() {
        let (clean, out, _) = run_case(&["-sc", "docs", "docs/sub/b"], &tree());
        assert!(clean);
        // `docs/sub/b` was counted inside `docs`, so the total is 6144 B.
        assert_eq!(out, "6\tdocs\n6\ttotal\n");
        let (_, out, _) = run_case(&["-sc", "docs/sub", "docs/a"], &tree());
        // Two operands apart: 4608 + 1024 = 5632 B.
        assert_eq!(out, "5\tdocs/sub\n1\tdocs/a\n6\ttotal\n");
    }

    #[test]
    fn max_depth_bounds_the_rows_not_the_sums() {
        let (clean, out, _) = run_case(&["-d0", "docs"], &tree());
        assert!(clean);
        assert_eq!(out, "6\tdocs\n");
    }

    #[test]
    fn separate_dirs_excludes_subdirectories_from_a_row() {
        let (clean, out, _) = run_case(&["-S", "docs"], &tree());
        assert!(clean);
        // docs' own row excludes sub's 4608 B: 512 + 1024 = 1536 B → 2.
        assert_eq!(out, "5\tdocs/sub\n2\tdocs\n");
    }

    #[test]
    fn apparent_size_and_bytes_measure_lengths() {
        let (clean, out, _) = run_case(&["-b", "docs"], &tree());
        assert!(clean);
        // Apparent bytes: sub = 0 + 3000; docs = 0 + 100 + 3000.
        assert_eq!(out, "3000\tdocs/sub\n3100\tdocs\n");
    }

    #[test]
    fn human_readable_scales_the_rows() {
        let (clean, out, _) = run_case(&["-h", "docs"], &tree());
        assert!(clean);
        assert_eq!(out, "4.5K\tdocs/sub\n6.0K\tdocs\n");
    }

    #[test]
    fn a_multiply_named_file_is_counted_once() {
        // One 4096-byte file under two names, plus a 1024-byte file and the
        // 512-byte directory: 512 + 4096 + 1024 = 5632 B → 6 blocks. Counting
        // the linked file per name would give 9728 B → 10 blocks.
        let (clean, out, err) = run_case(&["docs"], &linked_tree());
        assert!(clean);
        assert_eq!(out, "6\tdocs\n");
        assert!(err.is_empty());
    }

    #[test]
    fn count_links_counts_every_name() {
        // `-l` is the GNU opt-out: 512 + 4096 + 1024 + 4096 = 9728 B → 10.
        let (clean, out, err) = run_case(&["-l", "docs"], &linked_tree());
        assert!(clean);
        assert_eq!(out, "10\tdocs\n");
        assert!(err.is_empty());
    }

    #[test]
    fn a_second_name_for_a_counted_file_gets_no_row() {
        let (clean, out, _) = run_case(&["-a", "docs"], &linked_tree());
        assert!(clean);
        assert_eq!(out, "4\tdocs/one\n1\tdocs/other\n6\tdocs\n");
    }

    #[test]
    fn two_operands_naming_one_node_are_counted_once() {
        let (clean, out, err) = run_case(&["-c", "docs/one", "docs/two"], &linked_tree());
        assert!(clean);
        assert_eq!(out, "4\tdocs/one\n4\ttotal\n");
        assert!(err.is_empty());
    }

    #[test]
    fn a_repeated_or_nested_operand_is_counted_once() {
        let (clean, out, _) = run_case(&["-c", "docs/other", "docs/other"], &linked_tree());
        assert!(clean);
        assert_eq!(out, "1\tdocs/other\n1\ttotal\n");
        let (_, out, _) = run_case(&["-c", "docs", "docs/sub"], &tree());
        assert_eq!(out, "5\tdocs/sub\n6\tdocs\n6\ttotal\n");
        let (_, out, _) = run_case(&["-c", "-a", "docs/a", "docs"], &tree());
        assert_eq!(
            out,
            "1\tdocs/a\n4\tdocs/sub/b\n5\tdocs/sub\n5\tdocs\n6\ttotal\n"
        );
        let (_, out, _) = run_case(&["-l", "docs", "docs"], &tree());
        assert_eq!(out, "5\tdocs/sub\n6\tdocs\n5\tdocs/sub\n6\tdocs\n");
    }

    /// The `tree` fixture with links: `ld` names `docs`, `docs/fl` names
    /// `docs/a`, `docs/x` names `docs/sub`, `docs/sub/up` names `docs` and
    /// `docs/dl` names nothing.
    fn linked_up() -> MemFs {
        MemFs::new(&[
            (
                "docs",
                Node::Dir(
                    512,
                    alloc::vec![
                        "a".to_string(),
                        "fl".to_string(),
                        "sub".to_string(),
                        "x".to_string()
                    ],
                ),
            ),
            ("docs/a", Node::File(100, 1024)),
            ("docs/fl", Node::Symlink("docs/a".to_string())),
            (
                "docs/sub",
                Node::Dir(0, alloc::vec!["b".to_string(), "up".to_string()]),
            ),
            ("docs/sub/b", Node::File(3000, 4096)),
            ("docs/sub/up", Node::Symlink("docs".to_string())),
            ("docs/x", Node::Symlink("docs/sub".to_string())),
            ("ld", Node::Symlink("docs".to_string())),
        ])
    }

    #[test]
    fn a_link_operand_counts_as_itself_unless_followed() {
        let (_, out, _) = run_case(&["ld"], &linked_up());
        assert_eq!(out, "0\tld\n");
        for follow in ["-H", "-D", "--dereference-args", "-L", "--dereference"] {
            let (clean, out, _) = run_case(&[follow, "-s", "ld"], &linked_up());
            assert!(clean, "{follow}");
            assert_eq!(out, "6\tld\n", "{follow}");
        }
        let (_, out, _) = run_case(&["-L", "-P", "ld"], &linked_up());
        assert_eq!(out, "0\tld\n", "the last of -L and -P wins");
    }

    #[test]
    fn dereference_follows_every_link_and_counts_each_node_once() {
        let (clean, out, err) = run_case(&["-L", "-a", "docs"], &linked_up());
        assert!(clean, "{err}");
        assert_eq!(out, "1\tdocs/a\n4\tdocs/sub/b\n4\tdocs/sub\n6\tdocs\n");
    }

    #[test]
    fn a_link_back_up_is_never_walked_again_even_counting_every_name() {
        // `up` leads to `docs`, whose `sub` (and `x`) is the walk's own root:
        // 512 + 1024 (`a`) + 1024 (`fl`, a second name `-l` counts) = 2560 B
        // under `up`, and 4096 + 2560 = 6656 B for the operand.
        let (clean, out, _) = run_case(&["-L", "-l", "docs/sub"], &linked_up());
        assert!(clean);
        assert_eq!(out, "3\tdocs/sub/up\n7\tdocs/sub\n");
    }

    #[test]
    fn a_dangling_link_is_named_alone_under_dereference() {
        let fs = MemFs::new(&[
            ("d", Node::Dir(0, alloc::vec!["dl".to_string()])),
            ("d/dl", Node::Symlink("nowhere".to_string())),
        ]);
        let (clean, out, err) = run_case(&["-L", "d"], &fs);
        assert!(!clean);
        assert_eq!(out, "0\td\n");
        assert_eq!(err, "du: cannot access 'd/dl'\n");
    }

    #[test]
    fn a_node_with_no_identity_is_counted_and_reported() {
        // A multiply-named node the backing offers no identity for cannot be
        // keyed on one. Merging it would silently lose real storage, so it is
        // counted — and the run says so and exits non-zero rather than
        // reporting a wrong total as clean.
        let fs = MemFs::new(&[
            (
                "docs",
                Node::Dir(0, alloc::vec!["x".to_string(), "y".to_string()]),
            ),
            ("docs/x", Node::Unidentified(0, 1024, 2)),
            ("docs/y", Node::Unidentified(0, 1024, 2)),
        ]);
        let (clean, out, err) = run_case(&["docs"], &fs);
        assert!(!clean, "an over-count is not a clean run");
        assert_eq!(out, "2\tdocs\n");
        // Reported once, not once per entry.
        assert_eq!(
            err,
            "du: cannot track every hard link; multiply-named files are counted once per name\n"
        );
    }

    #[test]
    fn an_unreachable_operand_is_diagnosed_and_the_walk_continues() {
        let fs = MemFs::new(&[
            ("docs", Node::Dir(512, alloc::vec!["a".to_string()])),
            ("docs/a", Node::File(100, 1024)),
            ("gone", Node::Denied(Errno::PermissionDenied)),
        ]);
        let (clean, out, err) = run_case(&["gone", "docs"], &fs);
        assert!(!clean, "a diagnosed path makes the run unclean");
        // The reachable operand is still counted: 512 + 1024 = 1536 B → 2.
        assert_eq!(out, "2\tdocs\n");
        assert!(err.contains("gone"));
        assert!(err.contains("cannot access"));
    }

    #[test]
    fn an_unlistable_directory_is_diagnosed_and_skipped() {
        let fs = MemFs::new(&[
            ("docs", Node::Dir(512, alloc::vec!["locked".to_string()])),
            ("docs/locked", Node::Unlistable(512)),
        ]);
        let (clean, out, err) = run_case(&["docs"], &fs);
        assert!(!clean);
        // Only docs' own 512 B are countable → 1 block.
        assert_eq!(out, "1\tdocs\n");
        assert!(err.contains("docs/locked"));
    }

    #[test]
    fn a_missing_operand_is_diagnosed_without_a_row() {
        let (clean, out, err) = run_case(&["absent"], &tree());
        assert!(!clean);
        assert!(out.is_empty());
        assert!(err.contains("absent"));
    }

    #[test]
    fn no_operand_walks_the_current_directory() {
        let fs = MemFs::new(&[(".", Node::Dir(512, Vec::new()))]);
        let (clean, out, _) = run_case(&[], &fs);
        assert!(clean);
        assert_eq!(out, "1\t.\n");
    }

    #[test]
    fn a_file_operand_reports_one_row() {
        let (clean, out, _) = run_case(&["docs/a"], &tree());
        assert!(clean);
        assert_eq!(out, "1\tdocs/a\n");
    }

    #[test]
    fn help_prints_the_usage_fallback() {
        let out = MemOut::default();
        let err = MemOut::default();
        let clean = run(Command::Help, None, &tree(), &NoHelp, &out, &err, keys()).expect("run");
        assert!(clean);
        assert_eq!(out.text(), alloc::format!("{USAGE}\n"));
    }

    #[test]
    fn a_failed_output_write_is_fatal() {
        let command = parse(&["docs"]).expect("parse");
        let err = MemOut::default();
        let result = run(command, None, &tree(), &NoHelp, &BrokenSink, &err, keys());
        assert_eq!(result, Err(DuError::Output(Errno::NotImplemented)));
    }

    /// Every locale's `OPTIONS` section documents exactly the switches this
    /// parser accepts (`plans/APPS.md` §3.1): the flag tokens are
    /// language-neutral, so each translated document must carry the same
    /// keys as the canonical one. The documents are read from the bundle's
    /// own on-disk `Help/` tree — the single source the image builder
    /// plants — never a copy embedded in this crate.
    #[test]
    fn help_documents_the_parser_switches() {
        use alloc::format;
        use std::fs;

        let help_root = format!("{}/Help", env!("CARGO_MANIFEST_DIR"));
        let locales = tairix_help::REQUIRED_LOCALES;
        for locale in locales {
            let path = format!("{help_root}/{locale}/du.md");
            let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
            for switch in [
                "`-a, --all`",
                "`-s, --summarize`",
                "`-c, --total`",
                "`-h, --human-readable`",
                "`--si`",
                "`-k`",
                "`-m`",
                "`-b, --bytes`",
                "`-B, --block-size <size>`",
                "`--apparent-size`",
                "`-d, --max-depth <n>`",
                "`-S, --separate-dirs`",
                "`-l, --count-links`",
                "`-L, --dereference`",
                "`-H, -D, --dereference-args`",
                "`-P, --no-dereference`",
                "`-0, --null`",
                "`-?, --help`",
            ] {
                assert!(
                    text.contains(switch),
                    "{locale}/du.md must document {switch}"
                );
            }
        }
    }
}
