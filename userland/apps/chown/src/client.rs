//! The ownership-changing engine: each operand opened as the options say, a
//! tree walked through the directories it holds, and every change made
//! through the descriptor of the node its old owner was read from.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;

use tairix_abi::fs::{FileKind, FileStat, FS_OWNER_UNCHANGED};
use tairix_abi::Errno;
use tairix_fscopy::tree::{
    open_operand, untouched_link, walk, Dir, Entry, Kind, Node, Operand, Roots, Step, Unopened,
    ViewRoot, Visitor,
};
use tairix_help::{own_short_help, HelpSource};
use tairix_util::output::{report_failure, Carried};
use tairix_util::quote::quote;

use crate::command::{Command, Links, Options, Target, Traverse, Verbosity};
use crate::error::ChownError;
use crate::io::{Accounts, Output};
use crate::owner::{resolve, Owner};

/// The usage banner the short-help switches print when `chown`'s own Help
/// tree is unavailable.
pub const USAGE: &str = "\
usage: chown [OPTION]... [OWNER][:[GROUP]] file...
       chown [OPTION]... --reference=RFILE file...

  -c, --changes          report only files whose ownership actually changed
  -f, --silent, --quiet  suppress most error messages
  -v, --verbose          report every file processed
      --dereference      change what a symbolic link names (without -R, the
                         default)
  -h, --no-dereference   change a symbolic link itself
      --no-preserve-root treat '/' like any other directory (the default)
      --preserve-root    with -R, refuse to change '/'
      --from=CURRENT_OWNER:CURRENT_GROUP
                         change only a file owned as this says
      --reference=RFILE  use RFILE's owner and group
  -R, --recursive        change files and directories recursively
  -H                     with -R, follow a link operand that names a directory
  -L                     with -R, follow every link (refused)
  -P                     with -R, follow no link (the default)
  -?, --help             show this message
      --version          show the version

OWNER and GROUP are names or numbers, a leading + making one a number;
OWNER: also sets the owner's login group. `--` ends option parsing: every
later argument is an operand.
";

/// `chown`'s own command word.
const OWN_WORD: &str = "chown";

/// Run one [`Command`], changing owners through `roots` and reading names
/// through `accounts`; `locale` is the user's `LANG`, if set, and `help` the
/// tool's own `Help/` tree.
///
/// Every change is made through the descriptor its node's current owner was
/// read from, and `-R` walks a directory through the directories it holds,
/// changing it after its contents. A node that cannot be reached, listed or
/// changed is reported on `err` (silently under `-f`) and the rest are still
/// changed, answering `Ok(false)`; nothing failing answers `Ok(true)`. `-v`
/// and `-c` report on `out`.
///
/// # Errors
///
/// An owner, group or reference that cannot be read, before
/// anything changes, or [`ChownError::Output`] when a stream refuses a write.
pub fn run<R: Roots>(
    command: Command,
    locale: Option<&str>,
    roots: &R,
    accounts: &dyn Accounts,
    help: &dyn HelpSource,
    out: &dyn Output,
    err: &dyn Output,
) -> Result<bool, ChownError> {
    let (options, target, from, files) = match command {
        Command::Help => return short_help(locale, help, out).map(|()| true),
        Command::Version => {
            let line = format!("{OWN_WORD} (TAIRiX) {}\n", env!("CARGO_PKG_VERSION"));
            return out
                .write_all(line.as_bytes())
                .map(|()| true)
                .map_err(ChownError::Output);
        }
        Command::Change {
            options,
            target,
            from,
            files,
        } => (options, target, from, files),
    };
    let from = from
        .as_deref()
        .map(|spec| read_owner(spec, accounts, err))
        .transpose()?;
    let owner = match &target {
        Target::Spec(spec) => read_owner(spec, accounts, err)?,
        Target::Reference(path) => reference(roots, path)?,
    };
    let mut change = Change {
        owner,
        from,
        options,
        out,
        carried: Carried::new(err, OWN_WORD).silenced(options.quiet),
        names: Names {
            accounts,
            users: BTreeMap::new(),
            groups: BTreeMap::new(),
        },
        view_root: ViewRoot::default(),
    };
    for file in &files {
        change.operand(roots, file)?;
    }
    Ok(change.carried.clean())
}

/// `chown`'s own short help from its own Help tree, or the usage banner when
/// no document can be served.
fn short_help(
    locale: Option<&str>,
    help: &dyn HelpSource,
    out: &dyn Output,
) -> Result<(), ChownError> {
    let bytes = own_short_help(help, locale, OWN_WORD).unwrap_or_else(|| USAGE.as_bytes().to_vec());
    out.write_all(&bytes).map_err(ChownError::Output)
}

/// Resolve an owner operand, warning as GNU does of the older `.` separator.
fn read_owner(spec: &str, accounts: &dyn Accounts, err: &dyn Output) -> Result<Owner, ChownError> {
    let read = resolve(spec, accounts)?;
    if read.dotted {
        report_failure(err, OWN_WORD, "warning: '.' should be ':'").map_err(ChownError::Output)?;
    }
    Ok(read.owner)
}

/// The owner and group of what `path` names, read through a final link as
/// GNU's `--reference` reads them.
fn reference<R: Roots>(roots: &R, path: &str) -> Result<Owner, ChownError> {
    let stat = roots
        .open_node(path, true)
        .and_then(|node| node.stat())
        .and_then(|stat| {
            // Asked of the system, the word that leaves an id alone would
            // change nothing.
            if stat.uid == FS_OWNER_UNCHANGED || stat.gid == FS_OWNER_UNCHANGED {
                Err(Errno::OutOfRange)
            } else {
                Ok(stat)
            }
        })
        .map_err(|errno| ChownError::Reference {
            path: String::from(path),
            errno,
        })?;
    Ok(Owner {
        uid: Some(stat.uid),
        gid: Some(stat.gid),
    })
}

/// One run's change, carried across its operands and walks.
struct Change<'a> {
    owner: Owner,
    from: Option<Owner>,
    options: Options,
    out: &'a dyn Output,
    carried: Carried<'a>,
    names: Names<'a>,
    view_root: ViewRoot,
}

/// The names a report spells ids by, read once each: GNU names an owner or
/// a group by name where the directory lists one, by number otherwise.
struct Names<'a> {
    accounts: &'a dyn Accounts,
    users: BTreeMap<u32, String>,
    groups: BTreeMap<u32, String>,
}

impl Names<'_> {
    /// `owner` as a report spells it.
    fn spelled(&mut self, owner: Owner) -> String {
        match (owner.uid, owner.gid) {
            (Some(uid), Some(gid)) => format!("{}:{}", self.user(uid), self.group(gid)),
            (Some(uid), None) => self.user(uid),
            (None, Some(gid)) => self.group(gid),
            (None, None) => String::new(),
        }
    }

    fn user(&mut self, uid: u32) -> String {
        let accounts = self.accounts;
        named(&mut self.users, uid, |uid| accounts.user_name(uid))
    }

    fn group(&mut self, gid: u32) -> String {
        let accounts = self.accounts;
        named(&mut self.groups, gid, |gid| accounts.group_name(gid))
    }
}

/// The name `id` has in `known`, else what `find` reads, else the number: a
/// directory that cannot be read leaves the report its number, as GNU's does.
fn named(
    known: &mut BTreeMap<u32, String>,
    id: u32,
    find: impl FnOnce(u32) -> Result<Option<String>, Errno>,
) -> String {
    known
        .entry(id)
        .or_insert_with(|| find(id).ok().flatten().unwrap_or_else(|| format!("{id}")))
        .clone()
}

/// What became of a node the change reached.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Outcome {
    /// Its owner was set.
    Applied,
    /// Setting it was refused.
    Refused,
    /// Nothing was asked of it: `--from` declined it, or the operand names
    /// no id.
    Kept,
}

impl Change<'_> {
    /// Change the operand `path`, opened as `-h`, `--dereference`, `-R`, `-H`
    /// and `-P` say.
    fn operand<R: Roots>(&mut self, roots: &R, path: &str) -> Result<(), ChownError> {
        let links = self.options.links();
        if !self.options.recursive {
            return match links {
                Links::Referent => self.referent(roots, path),
                Links::Itself => self.named(roots.open_node(path, false), path),
            };
        }
        if self.options.traverse == Traverse::Operands {
            return match open_operand(roots, path) {
                Ok(Operand::Directory(dir)) => self.tree(roots, &dir, path),
                Ok(Operand::Node(node)) if links == Links::Referent => self.node(&node, path),
                Err(Unopened::Dangling) if links == Links::Referent => self.dangling(path),
                Err(Unopened::Refused(errno)) => self.unreachable(stat_failure(path, errno), path),
                Ok(Operand::Node(_)) | Err(Unopened::Dangling) => {
                    self.named(roots.open_node(path, false), path)
                }
            };
        }
        // Only a directory reached through no link is walked under `-P`.
        match roots.open_dir(path, false) {
            Ok(dir) => self.tree(roots, &dir, path),
            Err(Errno::NotADirectory | Errno::NotFound) if links == Links::Referent => {
                self.referent(roots, path)
            }
            Err(Errno::NotADirectory | Errno::NotFound) => {
                self.named(roots.open_node(path, false), path)
            }
            Err(errno) => self.unlisted(roots.open_node(path, false), path, errno),
        }
    }

    /// What the operand `path` names through a final link, changed and never
    /// walked.
    fn referent<R: Roots>(&mut self, roots: &R, path: &str) -> Result<(), ChownError> {
        match open_operand(roots, path) {
            Ok(Operand::Directory(dir)) => self.node(&dir, path),
            Ok(Operand::Node(node)) => self.node(&node, path),
            Err(Unopened::Dangling) => self.dangling(path),
            Err(Unopened::Refused(errno)) => self.unreachable(stat_failure(path, errno), path),
        }
    }

    /// Walk the directory `dir`, then change it: giving a directory away
    /// grants no reach into it, so changing it first could shut the walk out.
    fn tree<R: Roots>(&mut self, roots: &R, dir: &R::Dir, path: &str) -> Result<(), ChownError> {
        if self.options.preserve_root {
            let refusal = if ViewRoot::spelled(path) {
                Some(ChownError::PreserveRoot(String::from(path)))
            } else {
                match self.view_root.holds(roots, dir) {
                    Ok(false) => None,
                    Ok(true) => Some(ChownError::PreserveRoot(String::from(path))),
                    Err(errno) => Some(stat_failure(path, errno)),
                }
            };
            if let Some(refusal) = refusal {
                return self.carried.carry(refusal);
            }
        }
        walk(dir, path, self)?;
        self.node(dir, path)
    }

    /// What `opened` reached, a directory `errno` kept the walk out of: GNU
    /// still changes one it cannot list, and says why it could not.
    fn unlisted<N: Node>(
        &mut self,
        opened: Result<N, Errno>,
        path: &str,
        errno: Errno,
    ) -> Result<(), ChownError> {
        let Some((node, stat)) = self.read(opened, path)? else {
            return Ok(());
        };
        if stat.kind != FileKind::Directory {
            return self.met(&node, &stat, path);
        }
        self.carried.carry(ChownError::Read {
            path: String::from(path),
            errno,
        })?;
        self.apply(&node, &stat, path)
    }

    /// Change what `opened` reached, a failure to open it reported as an
    /// unreachable node.
    fn named<N: Node>(&mut self, opened: Result<N, Errno>, path: &str) -> Result<(), ChownError> {
        if let Some((node, stat)) = self.read(opened, path)? {
            self.apply(&node, &stat, path)?;
        }
        Ok(())
    }

    /// Change the node `node` holds.
    fn node<N: Node>(&mut self, node: &N, path: &str) -> Result<(), ChownError> {
        match node.stat() {
            Ok(stat) => self.apply(node, &stat, path),
            Err(errno) => self.unreachable(stat_failure(path, errno), path),
        }
    }

    /// `opened` with its status, or [`None`] once its failure is reported.
    fn read<N: Node>(
        &mut self,
        opened: Result<N, Errno>,
        path: &str,
    ) -> Result<Option<(N, FileStat)>, ChownError> {
        match opened.and_then(|node| node.stat().map(|stat| (node, stat))) {
            Ok(read) => Ok(Some(read)),
            Err(errno) => self
                .unreachable(stat_failure(path, errno), path)
                .map(|()| None),
        }
    }

    /// A node the walk met and does not descend: a link is changed itself,
    /// or under `--dereference` left alone with what it names.
    fn met<N: Node>(&mut self, node: &N, stat: &FileStat, path: &str) -> Result<(), ChownError> {
        if stat.kind == FileKind::Symlink && self.options.links() == Links::Referent {
            return self.leave_link(path);
        }
        self.apply(node, stat, path)
    }

    /// Change `node`, read as `stat`, unless `--from` declines it.
    fn apply<N: Node>(&mut self, node: &N, stat: &FileStat, path: &str) -> Result<(), ChownError> {
        let before = (stat.uid, stat.gid);
        let declined = self.from.is_some_and(|from| !from.matches(before));
        if declined || self.owner.is_empty() {
            return self.report(path, Some(before), Outcome::Kept);
        }
        let group = self.owner.uid.is_none();
        let set = node
            .set_owner(
                self.owner.uid.unwrap_or(FS_OWNER_UNCHANGED),
                self.owner.gid.unwrap_or(FS_OWNER_UNCHANGED),
            )
            .map_err(|errno| ChownError::Apply {
                path: String::from(path),
                errno,
                group,
            });
        let outcome = if self.carried.absorb(set)?.is_some() {
            Outcome::Applied
        } else {
            Outcome::Refused
        };
        self.report(path, Some(before), outcome)
    }

    /// Report an operand that is a link naming nothing.
    fn dangling(&mut self, path: &str) -> Result<(), ChownError> {
        self.unreachable(ChownError::Dangling(String::from(path)), path)
    }

    /// Report a node that could not be reached, then name it under `-v`.
    fn unreachable(&mut self, failure: ChownError, path: &str) -> Result<(), ChownError> {
        self.carried.carry(failure)?;
        self.report(path, None, Outcome::Refused)
    }

    /// Report one node as GNU's `-c` and `-v` do, `before` its `(uid, gid)`
    /// or [`None`] when it could not be reached.
    fn report(
        &mut self,
        path: &str,
        before: Option<(u32, u32)>,
        outcome: Outcome,
    ) -> Result<(), ChownError> {
        let wanted = match self.options.verbosity {
            Verbosity::None => false,
            Verbosity::Changes => {
                outcome == Outcome::Applied && before.is_some_and(|ids| !self.owner.matches(ids))
            }
            Verbosity::All => true,
        };
        if !wanted {
            return Ok(());
        }
        let line = describe(self.owner, path, before, outcome, &mut self.names);
        self.out
            .write_all(line.as_bytes())
            .map_err(ChownError::Output)
    }

    /// Say under `-v` that a link the walk met, and what it names, were left
    /// alone.
    fn leave_link(&self, path: &str) -> Result<(), ChownError> {
        if self.options.verbosity != Verbosity::All {
            return Ok(());
        }
        self.out
            .write_all(untouched_link(path).as_bytes())
            .map_err(ChownError::Output)
    }
}

impl<D: Dir> Visitor<D> for Change<'_> {
    type Error = ChownError;

    fn enter(&mut self, dir: &D, entry: &Entry, path: &str) -> Result<Step<D>, ChownError> {
        if entry.kind == Kind::Directory {
            return Ok(Step::Descend);
        }
        if let Some((node, stat)) = self.read(dir.open_node(&entry.name), path)? {
            self.met(&node, &stat, path)?;
        }
        Ok(Step::Skip)
    }

    fn leave(&mut self, _dir: &D, _entry: &Entry, path: &str, held: &D) -> Result<(), ChownError> {
        self.node(held, path)
    }

    fn refused(
        &mut self,
        dir: &D,
        entry: &Entry,
        path: &str,
        errno: Errno,
    ) -> Result<(), ChownError> {
        self.unlisted(dir.open_node(&entry.name), path, errno)
    }

    fn unlisted_root(&mut self, path: &str, errno: Errno) -> Result<(), ChownError> {
        self.carried.carry(ChownError::Read {
            path: String::from(path),
            errno,
        })
    }
}

/// The failure to reach `path`.
fn stat_failure(path: &str, errno: Errno) -> ChownError {
    ChownError::Stat {
        path: String::from(path),
        errno,
    }
}

/// GNU's report line for `path` — changed, retained, or failed — with its
/// ids in the shape `owner` names them.
fn describe(
    owner: Owner,
    path: &str,
    before: Option<(u32, u32)>,
    outcome: Outcome,
    names: &mut Names<'_>,
) -> String {
    let path = quote(path);
    if owner.is_empty() {
        return if before.is_some() {
            format!("ownership of {path} retained\n")
        } else {
            format!("failed to change ownership of {path}\n")
        };
    }
    let what = if owner.uid.is_some() {
        "ownership"
    } else {
        "group"
    };
    let to = names.spelled(owner);
    let Some((uid, gid)) = before else {
        return format!("failed to change {what} of {path} to {to}\n");
    };
    let from = names.spelled(Owner {
        uid: owner.uid.map(|_| uid),
        gid: owner.gid.map(|_| gid),
    });
    match outcome {
        Outcome::Refused => format!("failed to change {what} of {path} from {from} to {to}\n"),
        Outcome::Applied if !owner.matches((uid, gid)) => {
            format!("changed {what} of {path} from {from} to {to}\n")
        }
        Outcome::Applied | Outcome::Kept => format!("{what} of {path} retained as {from}\n"),
    }
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;
