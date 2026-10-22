//! Deterministic fuzz harness for the shell's word expansion: the lexer's
//! pattern groups, brace expansion, parameter expansion and filename
//! generation, driven end to end through `Shell::run_line` over an
//! in-memory tree.
//!
//! The invariants:
//!
//! * no line — however hostile its quoting, braces, brackets or groups, and
//!   under any combination of the glob options — panics or fails to finish;
//! * every command reaching the process host has a program name, and the
//!   words it carries stay within the expansion bound.
//!
//! A plain `cargo test` runs the [`SMOKE_ITERATIONS`] sweep once from a
//! fresh, logged seed; `cargo xtask fuzz` exports `TAIRIX_FUZZ_BUDGET_SECS`
//! to extend it to a wall-clock budget.

use std::cell::RefCell;
use std::collections::BTreeMap;

use tairix_abi::{Errno, FileKind};
use tairix_elsh::{
    Console, DirEntryInfo, DirLister, GlobFs, LaunchError, LaunchSpec, Pid, ProcessHost, Shell,
    Signal, WaitOutcome,
};
use tairix_fuzzseed::Prng;

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_ITERATIONS: u64 = 3_000;

/// The most argument text one launched command may carry: the startup-block
/// bound the expansion is held to.
const ARGV_LIMIT: usize = 1 << 24;

/// A small tree, every path absolute: directories, files, a hidden entry,
/// a link to a directory, a dangling link and a link loop.
struct Tree {
    nodes: BTreeMap<&'static str, Node>,
}

#[derive(Clone, Copy)]
enum Node {
    Dir,
    File,
    Link(&'static str),
}

impl Tree {
    fn new() -> Self {
        let mut nodes = BTreeMap::new();
        for (path, node) in [
            ("/", Node::Dir),
            ("/w", Node::Dir),
            ("/w/a.txt", Node::File),
            ("/w/b.rs", Node::File),
            ("/w/.hidden", Node::File),
            ("/w/d", Node::Dir),
            ("/w/d/e", Node::Dir),
            ("/w/d/e/f.o", Node::File),
            ("/w/d/x", Node::File),
            ("/w/link", Node::Link("/w/d")),
            ("/w/dangle", Node::Link("/nowhere")),
            ("/w/loop", Node::Link("/w/loop")),
        ] {
            nodes.insert(path, node);
        }
        Self { nodes }
    }

    /// The absolute spelling of `path` from the working directory `/w`,
    /// links followed — a final one only when `follow` — to a bounded depth.
    fn resolve(&self, path: &str, follow: bool) -> Result<(String, Node), Errno> {
        let mut pending: Vec<String> = if path.starts_with('/') {
            Vec::new()
        } else {
            vec![String::from("w")]
        };
        pending.extend(path.split('/').filter(|p| !p.is_empty()).map(String::from));
        pending.reverse();
        let mut current: Vec<String> = Vec::new();
        let mut node = Node::Dir;
        for _ in 0..64 {
            let Some(part) = pending.pop() else {
                return Ok((format!("/{}", current.join("/")), node));
            };
            match part.as_str() {
                "." => continue,
                ".." => {
                    current.pop();
                    node = Node::Dir;
                    continue;
                }
                _ => current.push(part),
            }
            let key = format!("/{}", current.join("/"));
            node = *self.nodes.get(key.as_str()).ok_or(Errno::NotFound)?;
            if let Node::Link(target) = node {
                if !pending.is_empty() || follow {
                    current.clear();
                    pending.extend(
                        target
                            .split('/')
                            .filter(|p| !p.is_empty())
                            .rev()
                            .map(String::from),
                    );
                    node = Node::Dir;
                }
            }
        }
        Err(Errno::LinkLoop)
    }
}

impl DirLister for Tree {
    fn list_dir(&self, dir: &str) -> Result<Vec<DirEntryInfo>, Errno> {
        let (key, node) = self.resolve(dir, true)?;
        if !matches!(node, Node::Dir) {
            return Err(Errno::NotADirectory);
        }
        let prefix = if key == "/" { key } else { format!("{key}/") };
        Ok(self
            .nodes
            .iter()
            .filter_map(|(path, node)| {
                let name = path.strip_prefix(prefix.as_str())?;
                (!name.is_empty() && !name.contains('/')).then(|| DirEntryInfo {
                    name: String::from(name),
                    kind: kind_of(*node),
                })
            })
            .collect())
    }
}

impl GlobFs for Tree {
    fn kind(&self, path: &str, follow: bool) -> Result<FileKind, Errno> {
        self.resolve(path, follow).map(|(_, node)| kind_of(node))
    }
}

fn kind_of(node: Node) -> FileKind {
    match node {
        Node::Dir => FileKind::Directory,
        Node::File => FileKind::Regular,
        Node::Link(_) => FileKind::Symlink,
    }
}

/// Output goes nowhere; only the invariants are checked.
struct Silent;

impl Console for Silent {
    fn write_stdout(&self, _text: &str) {}
    fn write_stderr(&self, _text: &str) {}
}

/// Accepts every launch, checking what it is handed.
struct Host {
    launches: RefCell<u64>,
}

impl ProcessHost for Host {
    fn launch(&self, spec: &LaunchSpec<'_>) -> Result<Pid, LaunchError> {
        for command in spec.commands {
            assert!(!command.argv.is_empty(), "a command with no program name");
            let text: usize = command.argv.iter().map(String::len).sum();
            assert!(text <= ARGV_LIMIT, "an argument vector past the bound");
        }
        *self.launches.borrow_mut() += 1;
        Ok(Pid::new(1))
    }

    fn wait(&self, _pid: Pid) -> Result<WaitOutcome, Errno> {
        Ok(WaitOutcome::Exited(0))
    }

    fn signal(&self, _pid: Pid, _signal: Signal) -> Result<(), Errno> {
        Ok(())
    }

    fn foreground(&self, _job: Option<Pid>) {}

    fn poll(&self) -> Option<(Pid, WaitOutcome)> {
        None
    }

    fn change_directory(&self, _path: &str) -> Result<String, Errno> {
        Err(Errno::NotFound)
    }
}

/// Pieces a hostile line is spliced from: every expansion's syntax, quoting,
/// and the operators around it.
const PIECES: &[&str] = &[
    "*",
    "?",
    "[",
    "]",
    "[!",
    "[:alpha:]",
    "[[:digit:]]",
    "{",
    "}",
    ",",
    "..",
    "{1..3}",
    "{a,b}",
    "{,}",
    "$",
    "$x",
    "${x}",
    "\"",
    "'",
    "\\",
    "@(",
    "*(",
    "+(",
    "!(",
    "?(",
    "|",
    ")",
    "/",
    "./",
    "../",
    "**",
    "**/",
    ".",
    "a",
    "d",
    "e",
    "link",
    "x",
    " ",
    ";",
    "&&",
    "<",
    ">",
    "Home:/",
    "sys:",
    "-",
    "=",
];

const OPTIONS: &[&str] = &[
    "dotglob",
    "extglob",
    "failglob",
    "globstar",
    "nocaseglob",
    "nullglob",
];

#[test]
fn expansion_never_panics_and_stays_bounded() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "expansion_never_panics_and_stays_bounded",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let tree = Tree::new();
    let console = Silent;
    let host = Host {
        launches: RefCell::new(0),
    };
    let mut iteration: u64 = 0;
    loop {
        let mut shell = Shell::new(&host, &console).with_filesystem(&tree);
        for option in OPTIONS {
            if rng.below(2) == 0 {
                let _ = shell.run_line(&format!("shopt -s {option}"));
            }
        }
        if rng.below(4) == 0 {
            let _ = shell.run_line("shopt -o -s noglob");
        }
        let _ = shell.run_line("x='*.txt'");
        match rng.below(4) {
            0 => {
                let _ = shell.run_line("GLOBIGNORE='*.o:d'");
            }
            1 => {
                let _ = shell.run_line("GLOBIGNORE='[z-a]:.'");
            }
            _ => {}
        }

        let mut line = String::from("cmd ");
        for _ in 0..rng.at_most(24) {
            line.push_str(rng.pick(PIECES));
        }
        let _ = shell.run_line(&line);

        let mut noise = vec![0u8; rng.at_most(96)];
        rng.fill(&mut noise);
        let _ = shell.run_line(&String::from_utf8_lossy(&noise));

        iteration += 1;
        if !tairix_fuzzseed::within_budget(deadline) && iteration >= SMOKE_ITERATIONS {
            break;
        }
    }
}
