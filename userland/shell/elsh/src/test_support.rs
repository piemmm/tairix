//! In-memory test doubles for the [`Console`] and [`ProcessHost`] seams.
//!
//! These fixtures let every unit and integration test drive the real shell
//! logic without a kernel: the [`RecordingConsole`] captures output, the
//! [`ScriptedHost`] answers `launch`/`wait`/`signal`/`change_directory` from a
//! programmable script while recording what it was asked to do, and the
//! [`Fixture`] bundles them behind a real [`BuiltinContext`] dispatch — the
//! one scaffolding every builtin's test module shares.

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::RefCell;

use tairix_abi::{Errno, FileKind, LimitKind, ResourceLimit};

use crate::builtin::{dispatch, BuiltinContext};
use crate::complete::{DirEntryInfo, DirLister};
use crate::env::Environment;
use crate::host::{
    Console, Elevator, LaunchError, LaunchSpec, LimitStore, ProcessHost, ResolvedCommand,
    NULL_ELEVATOR,
};
use crate::job::{JobTable, Pid, Signal, WaitOutcome};
use crate::options::ShellOptions;
use crate::pathexp::GlobFs;

/// A [`Console`] that accumulates everything written to each stream.
#[derive(Default)]
pub(crate) struct RecordingConsole {
    out: RefCell<String>,
    err: RefCell<String>,
}

impl RecordingConsole {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn stdout(&self) -> String {
        self.out.borrow().clone()
    }

    pub(crate) fn stderr(&self) -> String {
        self.err.borrow().clone()
    }

    pub(crate) fn clear(&self) {
        self.out.borrow_mut().clear();
        self.err.borrow_mut().clear();
    }
}

impl Console for RecordingConsole {
    fn write_stdout(&self, text: &str) {
        self.out.borrow_mut().push_str(text);
    }

    fn write_stderr(&self, text: &str) {
        self.err.borrow_mut().push_str(text);
    }
}

/// One thing the [`ScriptedHost`] was asked to launch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LaunchRecord {
    pub commands: Vec<ResolvedCommand>,
    pub env: Vec<(String, String)>,
    pub background: bool,
}

/// One job-control act the [`ScriptedHost`] was asked for.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum JobControl {
    /// The terminal handed to a job, or taken back with `None`.
    Foreground(Option<Pid>),
    /// A signal delivered to a job.
    Signal(Pid, Signal),
}

/// A programmable [`ProcessHost`].
pub(crate) struct ScriptedHost {
    next_pid: RefCell<u64>,
    launches: RefCell<Vec<LaunchRecord>>,
    fail_launch: RefCell<Option<(String, Errno)>>,
    fail_redirection: RefCell<Option<Errno>>,
    waits: RefCell<BTreeMap<u64, WaitOutcome>>,
    refuse_signals: RefCell<bool>,
    job_control: RefCell<Vec<JobControl>>,
    directories: RefCell<Vec<String>>,
    poll_queue: RefCell<Vec<(Pid, WaitOutcome)>>,
}

impl ScriptedHost {
    pub(crate) fn new() -> Self {
        Self {
            next_pid: RefCell::new(100),
            launches: RefCell::new(Vec::new()),
            fail_launch: RefCell::new(None),
            fail_redirection: RefCell::new(None),
            waits: RefCell::new(BTreeMap::new()),
            refuse_signals: RefCell::new(false),
            job_control: RefCell::new(Vec::new()),
            directories: RefCell::new(Vec::new()),
            poll_queue: RefCell::new(Vec::new()),
        }
    }

    /// Make the next `launch` whose `argv[0]` equals `name` fail with
    /// [`Errno::NotFound`].
    pub(crate) fn fail_launch_of(&self, name: &str) {
        self.fail_launch_with(name, Errno::NotFound);
    }

    /// Make the next `launch` whose `argv[0]` equals `name` fail with
    /// `errno` (e.g. [`Errno::PermissionDenied`] for a command that
    /// resolved but is refused).
    pub(crate) fn fail_launch_with(&self, name: &str, errno: Errno) {
        *self.fail_launch.borrow_mut() = Some((name.to_string(), errno));
    }

    /// Make every `launch` fail while opening a redirection target, as a
    /// host does for a missing input file or a refused resource reference.
    pub(crate) fn fail_redirection_with(&self, errno: Errno) {
        *self.fail_redirection.borrow_mut() = Some(errno);
    }

    /// Register the outcome `wait` returns for a given pid.
    pub(crate) fn set_wait(&self, pid: Pid, outcome: WaitOutcome) {
        self.waits.borrow_mut().insert(pid.as_u64(), outcome);
    }

    /// Mark a directory the host will accept in `change_directory`.
    pub(crate) fn set_directory(&self, path: &str) {
        self.directories.borrow_mut().push(path.to_string());
    }

    /// Queue a background state change for `poll` to report.
    pub(crate) fn queue_poll(&self, pid: Pid, outcome: WaitOutcome) {
        self.poll_queue.borrow_mut().push((pid, outcome));
    }

    pub(crate) fn launches(&self) -> Vec<LaunchRecord> {
        self.launches.borrow().clone()
    }

    /// Every job-control act so far, in the order it was asked for.
    pub(crate) fn job_control(&self) -> Vec<JobControl> {
        self.job_control.borrow().clone()
    }

    /// Make every `signal` fail, as for a job that has already gone.
    pub(crate) fn refuse_signals(&self) {
        *self.refuse_signals.borrow_mut() = true;
    }
}

impl ProcessHost for ScriptedHost {
    fn launch(&self, spec: &LaunchSpec<'_>) -> Result<Pid, LaunchError> {
        if let Some((name, errno)) = self.fail_launch.borrow().as_ref() {
            let first = spec.commands.first().and_then(|c| c.argv.first());
            if first == Some(name) {
                return Err(LaunchError::Spawn(*errno));
            }
        }
        if let Some(errno) = *self.fail_redirection.borrow() {
            return Err(LaunchError::Redirection(errno));
        }
        self.launches.borrow_mut().push(LaunchRecord {
            commands: spec.commands.to_vec(),
            env: spec
                .env
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
            background: spec.background,
        });
        let mut next = self.next_pid.borrow_mut();
        let pid = Pid::new(*next);
        *next += 1;
        Ok(pid)
    }

    fn wait(&self, pid: Pid) -> Result<WaitOutcome, Errno> {
        self.waits
            .borrow()
            .get(&pid.as_u64())
            .copied()
            .map_or(Ok(WaitOutcome::Exited(0)), Ok)
    }

    fn signal(&self, pid: Pid, signal: Signal) -> Result<(), Errno> {
        if *self.refuse_signals.borrow() {
            return Err(Errno::NotFound);
        }
        self.job_control
            .borrow_mut()
            .push(JobControl::Signal(pid, signal));
        Ok(())
    }

    fn foreground(&self, job: Option<Pid>) {
        self.job_control
            .borrow_mut()
            .push(JobControl::Foreground(job));
    }

    fn poll(&self) -> Option<(Pid, WaitOutcome)> {
        let mut queue = self.poll_queue.borrow_mut();
        if queue.is_empty() {
            None
        } else {
            Some(queue.remove(0))
        }
    }

    fn change_directory(&self, path: &str) -> Result<String, Errno> {
        if self.directories.borrow().iter().any(|d| d == path) {
            Ok(path.to_string())
        } else {
            Err(Errno::NotFound)
        }
    }
}

/// An in-memory [`LimitStore`]: a per-resource [`ResourceLimit`] map that lets
/// `ulimit` tests drive the real builtin logic without a kernel.
///
/// An unset resource reads back [`ResourceLimit::UNLIMITED`], matching the
/// kernel's default-unlimited starting point. [`put`] and
/// [`snapshot`] are test-only direct accessors that bypass the gating
/// [`set`](LimitStore::set) applies, so a test can arrange or inspect state;
/// [`deny_set`] makes the next [`set`](LimitStore::set) fail with a chosen
/// [`Errno`] to exercise the kernel-side raise gate (`CAP_RLIMIT_RAISE`).
///
/// [`put`]: MemoryLimitStore::put
/// [`snapshot`]: MemoryLimitStore::snapshot
/// [`deny_set`]: MemoryLimitStore::deny_set
pub(crate) struct MemoryLimitStore {
    limits: RefCell<BTreeMap<u32, ResourceLimit>>,
    deny: RefCell<Option<Errno>>,
}

impl MemoryLimitStore {
    pub(crate) fn new() -> Self {
        Self {
            limits: RefCell::new(BTreeMap::new()),
            deny: RefCell::new(None),
        }
    }

    /// Seed the stored limit for `kind` directly (test setup; bypasses the
    /// raise gate `set` applies).
    pub(crate) fn put(&self, kind: LimitKind, limit: ResourceLimit) {
        self.limits.borrow_mut().insert(kind.as_u32(), limit);
    }

    /// The currently-stored limit for `kind`, or [`ResourceLimit::UNLIMITED`]
    /// if none was set.
    pub(crate) fn snapshot(&self, kind: LimitKind) -> ResourceLimit {
        self.limits
            .borrow()
            .get(&kind.as_u32())
            .copied()
            .unwrap_or(ResourceLimit::UNLIMITED)
    }

    /// Make the next [`set`](LimitStore::set) fail closed with `errno`,
    /// modelling the kernel refusing to raise a hard bound.
    pub(crate) fn deny_set(&self, errno: Errno) {
        *self.deny.borrow_mut() = Some(errno);
    }
}

impl LimitStore for MemoryLimitStore {
    fn get(&self, kind: LimitKind) -> Result<ResourceLimit, Errno> {
        Ok(self.snapshot(kind))
    }

    fn set(&self, kind: LimitKind, value: ResourceLimit) -> Result<(), Errno> {
        if let Some(errno) = *self.deny.borrow() {
            return Err(errno);
        }
        self.put(kind, value);
        Ok(())
    }
}

/// The one builtin test fixture: real [`Environment`] and
/// [`JobTable`] state over the in-memory seams, dispatching
/// through the production [`BuiltinContext`] path.
pub(crate) struct Fixture<'a> {
    pub env: Environment,
    pub jobs: JobTable,
    pub host: ScriptedHost,
    pub console: RecordingConsole,
    pub limits: MemoryLimitStore,
    pub elevator: &'a dyn Elevator,
    pub options: ShellOptions,
    pub exit: Option<i32>,
}

impl Fixture<'static> {
    /// A fixture with the fail-closed default [`Elevator`].
    pub(crate) fn new() -> Self {
        Self::with_elevator(&NULL_ELEVATOR)
    }
}

impl<'a> Fixture<'a> {
    /// A fixture whose `elevate` builtin drives `elevator`.
    pub(crate) fn with_elevator(elevator: &'a dyn Elevator) -> Self {
        Self {
            env: Environment::new(),
            jobs: JobTable::new(),
            host: ScriptedHost::new(),
            console: RecordingConsole::new(),
            limits: MemoryLimitStore::new(),
            elevator,
            options: ShellOptions::default(),
            exit: None,
        }
    }

    /// Dispatch `words` (the full argv, name first) as a builtin line,
    /// returning its status — `None` when `words[0]` is not a builtin.
    pub(crate) fn run(&mut self, words: &[&str]) -> Option<i32> {
        let argv: Vec<String> = words.iter().map(|w| (*w).to_string()).collect();
        let mut ctx = BuiltinContext {
            env: &mut self.env,
            jobs: &mut self.jobs,
            host: &self.host,
            console: &self.console,
            limits: &self.limits,
            elevator: self.elevator,
            options: &mut self.options,
            exit: &mut self.exit,
        };
        dispatch(&mut ctx, &argv)
    }
}

/// A [`crate::complete::DirLister`] with no filesystem: every listing is
/// refused, so completion degrades to the non-filesystem candidates. The
/// default lister for tests that do not exercise completion.
pub(crate) struct EmptyLister;

impl crate::complete::DirLister for EmptyLister {
    fn list_dir(&self, _dir: &str) -> Result<Vec<crate::complete::DirEntryInfo>, Errno> {
        Err(Errno::NotImplemented)
    }
}

/// A resource lister with nothing to offer in any domain.
///
/// The REPL and editor tests are about the loop, the key handling, and the
/// candidate mechanics — not about which interfaces or interrupt lines a
/// machine has — so they run with no live selector names, which is also the
/// shape of a session holding no enumeration capability.
pub(crate) struct EmptyResources;

impl crate::complete::ResourceLister for EmptyResources {
    fn list(&self, _domain: tairix_resref::SelectorDomain) -> Result<Vec<String>, Errno> {
        Ok(Vec::new())
    }
}

/// One node of a [`MemFs`] tree.
#[derive(Clone, Debug)]
enum MemNode {
    Dir,
    File,
    /// A symbolic link and the path it holds.
    Link(String),
}

/// An in-memory file tree for filename-generation tests: directories,
/// files and links, reached through the same spellings the shell builds —
/// relative to a working directory, from `/`, or from an alias root. A
/// listing comes back in reverse name order, so a test only passes if the
/// shell sorts.
pub(crate) struct MemFs {
    cwd: String,
    aliases: Vec<(String, String)>,
    nodes: BTreeMap<String, MemNode>,
    failing: Vec<(String, Errno)>,
    interrupted: Vec<(String, Errno)>,
    listings: RefCell<usize>,
}

/// Longest chain of links [`MemFs`] follows before reporting a loop.
const MEM_LINK_HOPS: usize = 16;

impl MemFs {
    /// An empty tree whose working directory is `cwd` (absolute).
    pub(crate) fn new(cwd: &str) -> Self {
        let mut fs = Self {
            cwd: cwd.to_string(),
            aliases: Vec::new(),
            nodes: BTreeMap::new(),
            failing: Vec::new(),
            interrupted: Vec::new(),
            listings: RefCell::new(0),
        };
        fs.nodes.insert("/".to_string(), MemNode::Dir);
        fs.make_parents(cwd);
        fs.nodes.insert(cwd.to_string(), MemNode::Dir);
        fs
    }

    fn key(&self, path: &str) -> String {
        if path.starts_with('/') {
            path.to_string()
        } else if self.cwd == "/" {
            alloc::format!("/{path}")
        } else {
            alloc::format!("{}/{path}", self.cwd)
        }
    }

    fn make_parents(&mut self, key: &str) {
        let mut at = String::new();
        let parts: Vec<&str> = key.split('/').filter(|p| !p.is_empty()).collect();
        for part in parts.iter().take(parts.len().saturating_sub(1)) {
            at.push('/');
            at.push_str(part);
            self.nodes.entry(at.clone()).or_insert(MemNode::Dir);
        }
    }

    fn insert(mut self, path: &str, node: MemNode) -> Self {
        let key = self.key(path);
        self.make_parents(&key);
        self.nodes.insert(key, node);
        self
    }

    /// Add a directory (and its parents).
    pub(crate) fn dir(self, path: &str) -> Self {
        self.insert(path, MemNode::Dir)
    }

    /// Add a regular file (and its parents).
    pub(crate) fn file(self, path: &str) -> Self {
        self.insert(path, MemNode::File)
    }

    /// Add a link holding `target`.
    pub(crate) fn link(self, path: &str, target: &str) -> Self {
        self.insert(path, MemNode::Link(target.to_string()))
    }

    /// Spell the directory `path` as the alias root `name:/`.
    pub(crate) fn alias(mut self, name: &str, path: &str) -> Self {
        self.aliases.push((name.to_string(), path.to_string()));
        self
    }

    /// Make listing the directory `path` fail with `errno`.
    pub(crate) fn failing(mut self, path: &str, errno: Errno) -> Self {
        let key = self.key(path);
        self.failing.push((key, errno));
        self
    }

    /// Make listing the directory `path` fail with `errno` once its entries
    /// have been handed over.
    pub(crate) fn interrupted(mut self, path: &str, errno: Errno) -> Self {
        let key = self.key(path);
        self.interrupted.push((key, errno));
        self
    }

    /// How many directories have been listed.
    pub(crate) fn listings(&self) -> usize {
        *self.listings.borrow()
    }

    /// The components of `path` from the tree's root, aliases and the
    /// working directory applied, its root read by the path grammar's own
    /// rule.
    fn components(&self, path: &str) -> Result<Vec<String>, Errno> {
        let (base, rest) = if let Some(rest) = path.strip_prefix('/') {
            (String::new(), rest)
        } else if tairix_path::is_relative_spelling(path) {
            (self.cwd.clone(), path)
        } else {
            let len = tairix_path::alias_root_len(path).ok_or(Errno::NotFound)?;
            let name = &path[..len - ":/".len()];
            let (_, target) = self
                .aliases
                .iter()
                .find(|(alias, _)| alias == name)
                .ok_or(Errno::NotFound)?;
            (target.clone(), &path[len..])
        };
        let mut parts: Vec<String> = base
            .split('/')
            .filter(|p| !p.is_empty())
            .map(String::from)
            .collect();
        let pieces: Vec<&str> = rest.split('/').collect();
        for (index, piece) in pieces.iter().enumerate() {
            if piece.is_empty() {
                // An interior `//` is no path, as the kernel's grammar reads
                // it; a trailing `/` is allowed.
                if index + 1 == pieces.len() {
                    continue;
                }
                return Err(Errno::OutOfRange);
            }
            parts.push((*piece).to_string());
        }
        Ok(parts)
    }

    /// Resolve `path`, links in the way followed and a final one followed
    /// when `follow`, to its key and node.
    fn lookup(&self, path: &str, follow: bool) -> Result<(String, MemNode), Errno> {
        let mut pending = self.components(path)?;
        pending.reverse();
        let mut current: Vec<String> = Vec::new();
        let mut hops = 0;
        let mut node = MemNode::Dir;
        while let Some(part) = pending.pop() {
            match part.as_str() {
                "." => continue,
                ".." => {
                    current.pop();
                    node = MemNode::Dir;
                    continue;
                }
                _ => {}
            }
            current.push(part);
            let key = alloc::format!("/{}", current.join("/"));
            node = self.nodes.get(&key).cloned().ok_or(Errno::NotFound)?;
            if let MemNode::Link(target) = &node {
                if !pending.is_empty() || follow {
                    hops += 1;
                    if hops > MEM_LINK_HOPS {
                        return Err(Errno::LinkLoop);
                    }
                    current.pop();
                    if target.starts_with('/') {
                        current.clear();
                    }
                    pending.extend(
                        target
                            .split('/')
                            .filter(|p| !p.is_empty())
                            .rev()
                            .map(String::from),
                    );
                    node = MemNode::Dir;
                    continue;
                }
            }
            if !pending.is_empty() && !matches!(node, MemNode::Dir) {
                return Err(Errno::NotADirectory);
            }
        }
        Ok((alloc::format!("/{}", current.join("/")), node))
    }
}

impl DirLister for MemFs {
    fn list_dir(&self, dir: &str) -> Result<Vec<DirEntryInfo>, Errno> {
        *self.listings.borrow_mut() += 1;
        let (key, node) = self.lookup(dir, true)?;
        if let Some((_, errno)) = self.failing.iter().find(|(path, _)| *path == key) {
            return Err(*errno);
        }
        if !matches!(node, MemNode::Dir) {
            return Err(Errno::NotADirectory);
        }
        let prefix = if key == "/" {
            key.clone()
        } else {
            alloc::format!("{key}/")
        };
        let mut entries: Vec<DirEntryInfo> = self
            .nodes
            .iter()
            .filter_map(|(path, node)| {
                let name = path.strip_prefix(&prefix)?;
                (!name.is_empty() && !name.contains('/')).then(|| DirEntryInfo {
                    name: name.to_string(),
                    kind: match node {
                        MemNode::Dir => FileKind::Directory,
                        MemNode::File => FileKind::Regular,
                        MemNode::Link(_) => FileKind::Symlink,
                    },
                })
            })
            .collect();
        entries.reverse();
        Ok(entries)
    }
}

impl GlobFs for MemFs {
    fn read_dir(
        &self,
        dir: &str,
        each: &mut dyn FnMut(&str, FileKind) -> core::ops::ControlFlow<()>,
    ) -> Result<(), Errno> {
        for entry in self.list_dir(dir)? {
            if each(&entry.name, entry.kind).is_break() {
                return Ok(());
            }
        }
        let (key, _) = self.lookup(dir, true)?;
        match self.interrupted.iter().find(|(path, _)| *path == key) {
            Some((_, errno)) => Err(*errno),
            None => Ok(()),
        }
    }

    fn kind(&self, path: &str, follow: bool) -> Result<FileKind, Errno> {
        let (_, node) = self.lookup(path, follow)?;
        Ok(match node {
            MemNode::Dir => FileKind::Directory,
            MemNode::File => FileKind::Regular,
            MemNode::Link(_) => FileKind::Symlink,
        })
    }
}
