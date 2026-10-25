//! The removal engine: describe each operand, walk each directory `-r` must
//! remove through the directories it holds open, and unlink every reachable
//! object — depth-first, contents before the directory that holds them.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::{Access, Errno, FileId, FileKind, FileStat, NodeExpect, UnlinkFlags};
use tairix_fscopy::tree::{
    entry_name, walk, Dir, Entry, Kind, Node, Roots, Step, ViewRoot, Visitor,
};
use tairix_help::{own_short_help, HelpSource};
use tairix_path::leaf_name;
use tairix_util::output::Carried;
use tairix_util::quote::quote;

use crate::command::{Command, Interactive, Options};
use crate::error::RmError;
use crate::io::{Output, Prompt};

/// The usage banner the short-help switches print when `rm`'s own Help tree
/// is unavailable.
pub const USAGE: &str = "\
usage: rm [-dfiIrRv] [--] file...

  -r, -R, --recursive  remove directories and their contents
  -f, --force          ignore operands that do not exist; never prompt
  -d, --dir            remove empty directories
  -i, --interactive    prompt before every removal
  -I                   prompt once before removing more than three
                       operands, or before a recursive removal
  -v, --verbose        report each removal
  --preserve-root      refuse to remove '/' (the default)
  --no-preserve-root   allow removing '/'
  -h, -?, --help       show this message

At least one file operand is required unless -f is given. `--` ends option
parsing: every later argument is a path.
";

/// `rm`'s own command word: the short-help switches render its own Help
/// document through the same engine as any other command's.
const OWN_WORD: &str = "rm";

/// How an empty or emptied directory is removed: as a directory only, and
/// only while its name still holds the node the run read.
const EMPTIED: UnlinkFlags = UnlinkFlags::DIRECTORY.union(UnlinkFlags::EXPECT_NODE);

/// Run one [`Command`], removing its operands through `roots`. `locale` is
/// the user's `LANG` preference, if set; `help` is the tool's own `Help/`
/// tree, read by the short-help switches.
///
/// Operands are removed in order. A non-directory operand — a link as
/// itself — is unlinked; a directory operand is removed only with `-r`
/// (which removes its contents depth-first and then the directory itself)
/// or, when empty, with `-d`; under either, an operand whose last component
/// is `.` or `..` is refused before anything is looked up, as POSIX requires.
/// A tree is walked through the directories the run holds open, each entry
/// reached by name from its own directory and a link met there removed as
/// itself, never descended; a directory goes only while its name still
/// holds the node the walk emptied. With `-f` an operand that does not exist
/// is skipped silently. `-i` asks through `prompt` before every removal,
/// naming the object's kind as GNU does, and before descending into a
/// directory that holds anything; `-I` asks once up front for a large or
/// recursive removal. Without `-f`, when the prompt's replies come from a
/// terminal, a file the caller may not write is asked about as
/// write-protected before it is removed, a link never; under `-i` the
/// question names the protection. A declined question keeps the object (or
/// skips the whole run for `-I`) without error. `-v` reports each removal on
/// `out`; otherwise `rm` writes nothing on success beyond the
/// [`Command::Help`] banner. `--preserve-root` (the default) refuses a
/// recursive removal of the view root, however it is spelled.
///
/// A path that cannot be inspected, read or removed is reported on `err` in
/// GNU's words and the rest are still removed, the rest of its directory
/// too; a directory left holding something is then kept without a word, as
/// GNU keeps it. The run answers `Ok(false)` when anything failed and
/// `Ok(true)` otherwise.
///
/// # Errors
///
/// The failures that end the run: [`RmError::Prompt`] — a confirmation
/// could not be read (never treated as consent) — and [`RmError::Output`] —
/// writing the usage banner, a `-v` report or a diagnostic failed. A failed
/// operand ([`RmError::IsDirectory`], [`RmError::Dot`],
/// [`RmError::PreserveRoot`], [`RmError::Stat`] — suppressed for a missing
/// operand under `-f` — [`RmError::Read`], [`RmError::Remove`]) is reported
/// instead.
pub fn run<R: Roots>(
    command: Command,
    locale: Option<&str>,
    roots: &R,
    prompt: &dyn Prompt,
    help: &dyn HelpSource,
    out: &dyn Output,
    err: &dyn Output,
) -> Result<bool, RmError> {
    match command {
        Command::Help => short_help(locale, help, out).map(|()| true),
        Command::Remove { options, paths } => {
            if options.interactive == Interactive::Once && (paths.len() > 3 || options.recursive) {
                let plural = if paths.len() == 1 { "" } else { "s" };
                let recursively = if options.recursive {
                    " recursively"
                } else {
                    ""
                };
                let question = format!("remove {} argument{plural}{recursively}?", paths.len());
                if !prompt.confirm(&question).map_err(RmError::Prompt)? {
                    return Ok(true);
                }
            }
            let mut removing = Removing {
                options,
                roots,
                prompt,
                out,
                carried: Carried::new(err, OWN_WORD),
                at_terminal: !options.force && prompt.at_terminal(),
                whole: Vec::new(),
                unlisted: None,
                view_root: ViewRoot::default(),
            };
            for path in &paths {
                removing.operand(path)?;
            }
            Ok(removing.carried.clean())
        }
    }
}

/// Render `rm`'s own short help (`NAME` + `SYNOPSIS` + compact `OPTIONS`)
/// from its own Help tree through the one shared engine; when no document
/// can be served (a build without the bundle's documents) the usage banner
/// stands in — the tool's own text, not fabricated help content — so `-h`
/// never fails.
fn short_help(
    locale: Option<&str>,
    help: &dyn HelpSource,
    out: &dyn Output,
) -> Result<(), RmError> {
    let bytes =
        own_short_help(help, locale, OWN_WORD).unwrap_or_else(|| String::from(USAGE).into_bytes());
    out.write_all(&bytes).map_err(RmError::Output)
}

/// One removal run: its options, its seams, and the failures it carries past.
struct Removing<'a, R> {
    options: Options,
    roots: &'a R,
    prompt: &'a dyn Prompt,
    out: &'a dyn Output,
    carried: Carried<'a>,
    /// Without `-f`, whether the prompt's replies come from a terminal.
    at_terminal: bool,
    /// For each directory the walk is inside, whether everything it held went.
    whole: Vec<bool>,
    /// Why the walk's own root could not be listed.
    unlisted: Option<Errno>,
    view_root: ViewRoot,
}

/// Whether a removal took everything it was asked to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Removed {
    /// The object is gone.
    Whole,
    /// The object stays: declined, refused, or holding something that does.
    Kept,
}

/// Where an operand's own name is.
enum Place<D> {
    /// `name` in `dir`, which the run holds open.
    In { dir: D, name: String },
    /// None: a root, `.` or `..`, which no entry stands for.
    Unnamed,
}

/// What a removal knows of the node it is to remove.
#[derive(Clone, Copy)]
enum Known<'s> {
    /// It was read as `FileStat`.
    Stat(&'s FileStat),
    /// Its directory's listing named it so.
    Listed(FileId),
}

impl Known<'_> {
    const fn id(self) -> FileId {
        match self {
            Self::Stat(stat) => stat.id,
            Self::Listed(id) => id,
        }
    }
}

/// Under `-i`, what becomes of a directory before any descent into it.
enum Descent {
    /// It holds something and the descent was agreed to.
    Into,
    /// It holds nothing, so it is asked about as a removal instead.
    Empty,
    /// The descent was declined, or the directory could not be read.
    Declined,
}

impl<R: Roots> Removing<'_, R> {
    /// Remove one named operand, honouring POSIX's refusal of `.` and `..`,
    /// `-f` for a missing path, and the `--preserve-root` failsafe.
    fn operand(&mut self, path: &str) -> Result<(), RmError> {
        if (self.options.recursive || self.options.dir) && names_dot(path) {
            return self.carried.carry(RmError::Dot(String::from(path)));
        }
        if self.options.recursive && self.options.preserve_root && ViewRoot::spelled(path) {
            return self
                .carried
                .carry(RmError::PreserveRoot(String::from(path)));
        }
        if path.is_empty() {
            return self.unreached(path, Errno::NotFound);
        }
        let place = if entry_name(path).is_none() {
            Place::Unnamed
        } else {
            match self.roots.parent(path) {
                Ok((dir, name)) => Place::In { dir, name },
                Err(errno) => return self.unreached(path, errno),
            }
        };
        let stat = match self.describe(&place, path) {
            Ok(stat) => stat,
            Err(errno) => return self.unreached(path, errno),
        };
        if stat.kind != FileKind::Directory {
            let link = stat.kind == FileKind::Symlink;
            return self
                .by_name(&place, path, |this, dir, name| {
                    this.remove_file(dir, name, path, link, Known::Stat(&stat))
                })
                .map(drop);
        }
        if !self.options.recursive {
            if !self.options.dir {
                return self.carried.carry(RmError::IsDirectory(String::from(path)));
            }
            return self
                .by_name(&place, path, |this, dir, name| {
                    this.remove_directory(dir, name, path, Ok(stat))
                })
                .map(drop);
        }
        self.remove_tree(&place, path).map(drop)
    }

    /// What the operand at `place` names, a final link read as itself.
    fn describe(&self, place: &Place<R::Dir>, path: &str) -> Result<FileStat, Errno> {
        match place {
            Place::In { dir, name } => dir.open_node(name)?.stat(),
            Place::Unnamed => self.roots.open_node(path, false)?.stat(),
        }
    }

    /// An operand that could not be reached or described for `errno`: under
    /// `-f` a missing one is no failure.
    fn unreached(&mut self, path: &str, errno: Errno) -> Result<(), RmError> {
        if errno == Errno::NotFound && self.options.force {
            return Ok(());
        }
        self.carried.carry(RmError::Stat {
            path: String::from(path),
            errno,
        })
    }

    /// Run `act` on the directory holding the operand at `place` and its name
    /// there. No directory holds an unnamed one, so none can remove it: it is
    /// busy, as GNU finds `/`.
    fn by_name(
        &mut self,
        place: &Place<R::Dir>,
        path: &str,
        act: impl FnOnce(&mut Self, &R::Dir, &str) -> Result<Removed, RmError>,
    ) -> Result<Removed, RmError> {
        match place {
            Place::In { dir, name } => act(self, dir, name.as_str()),
            Place::Unnamed => {
                self.carried.carry(RmError::Remove {
                    path: String::from(path),
                    errno: Errno::Busy,
                })?;
                Ok(Removed::Kept)
            }
        }
    }

    /// Remove the non-directory `name` of `dir`, at `path` — a link when
    /// `link` — asking first where a question is due, and only while the name
    /// holds the node `known` describes. A failure is reported and keeps it.
    fn remove_file(
        &mut self,
        dir: &R::Dir,
        name: &str,
        path: &str,
        link: bool,
        known: Known<'_>,
    ) -> Result<Removed, RmError> {
        let Some(protected) = self.protection(dir, name, path, link)? else {
            return Ok(Removed::Kept);
        };
        let mut id = known.id();
        if protected || self.options.interactive == Interactive::Always {
            let read = match known {
                Known::Stat(stat) => Ok(*stat),
                Known::Listed(_) => dir.open_node(name).and_then(|node| node.stat()),
            };
            let stat = match read {
                Ok(stat) => stat,
                Err(errno) => {
                    self.carried.carry(RmError::Stat {
                        path: String::from(path),
                        errno,
                    })?;
                    return Ok(Removed::Kept);
                }
            };
            let marked = if protected { "write-protected " } else { "" };
            let question = format!("remove {marked}{} {}?", described(&stat), quote(path));
            if !self.prompt.confirm(&question).map_err(RmError::Prompt)? {
                return Ok(Removed::Kept);
            }
            id = stat.id;
        }
        let unlinked = if id.is_none() {
            dir.unlink(name, UnlinkFlags::empty(), None)
        } else {
            dir.unlink(
                name,
                UnlinkFlags::EXPECT_NODE,
                Some(&NodeExpect::of_node(id)),
            )
        };
        self.unlinked(unlinked, path, false)
    }

    /// Whether the non-directory `name` of `dir`, at `path`, is to be asked
    /// about as write-protected: never under `-f` or for a link, and only
    /// where a question is asked — at a terminal, or under `-i`, which asks
    /// anyway. `None` when the answer cannot be had, which is reported and
    /// keeps the file: one is never removed unasked on a guess.
    fn protection(
        &mut self,
        dir: &R::Dir,
        name: &str,
        path: &str,
        link: bool,
    ) -> Result<Option<bool>, RmError> {
        let asked = !self.options.force
            && !link
            && (self.at_terminal || self.options.interactive == Interactive::Always);
        if !asked {
            return Ok(Some(false));
        }
        match dir.access(name, Access::WRITE) {
            Err(Errno::PermissionDenied) => Ok(Some(true)),
            // A read-only volume refuses the removal itself; it is no
            // protection of the file's own.
            Ok(()) | Err(Errno::ReadOnly) => Ok(Some(false)),
            Err(errno) => {
                self.carried.carry(RmError::Stat {
                    path: String::from(path),
                    errno,
                })?;
                Ok(None)
            }
        }
    }

    /// Remove the directory operand at `place`, at `path`, and everything in
    /// it, contents first, through the directory the run holds open.
    fn remove_tree(&mut self, place: &Place<R::Dir>, path: &str) -> Result<Removed, RmError> {
        let opened = match place {
            Place::In { dir, name } => dir.open_dir(name),
            Place::Unnamed => self.roots.open_dir(path, false),
        };
        let held = match opened {
            Ok(held) => held,
            Err(errno) => return self.unreadable_at(place, path, errno, None),
        };
        if self.options.preserve_root {
            let refusal = match self.view_root.holds(self.roots, &held) {
                Ok(false) => None,
                Ok(true) => Some(RmError::PreserveRoot(String::from(path))),
                Err(errno) => Some(RmError::Stat {
                    path: String::from(path),
                    errno,
                }),
            };
            if let Some(refusal) = refusal {
                self.carried.carry(refusal)?;
                return Ok(Removed::Kept);
            }
        }
        if self.options.interactive == Interactive::Always {
            match self.descent(&held, path)? {
                Descent::Into => {}
                Descent::Empty => {
                    return self.by_name(place, path, |this, dir, name| {
                        this.remove_directory(dir, name, path, held.stat())
                    });
                }
                Descent::Declined => return Ok(Removed::Kept),
            }
        }
        let depth = self.whole.len();
        self.whole.push(true);
        self.unlisted = None;
        let walked = walk(&held, path, self);
        let whole = self.whole.get(depth) == Some(&true);
        self.whole.truncate(depth);
        walked?;
        if let Some(errno) = self.unlisted.take() {
            return self.unreadable_at(place, path, errno, Some(&held));
        }
        if !whole {
            // GNU keeps an ancestor of what stays without asking or reporting.
            return Ok(Removed::Kept);
        }
        self.by_name(place, path, |this, dir, name| {
            this.remove_directory(dir, name, path, held.stat())
        })
    }

    /// Under `-i`, whether to descend into the directory `held`, at `path`:
    /// GNU asks only of one that holds something, an empty one being asked
    /// about as a removal instead.
    fn descent(&mut self, held: &R::Dir, path: &str) -> Result<Descent, RmError> {
        let entries = match held.entries() {
            Ok(entries) => entries,
            Err(errno) => {
                self.carried.carry(unread(path, errno))?;
                return Ok(Descent::Declined);
            }
        };
        if entries.is_empty() {
            return Ok(Descent::Empty);
        }
        if self.confirmed(&format!("descend into directory {}?", quote(path)))? {
            Ok(Descent::Into)
        } else {
            Ok(Descent::Declined)
        }
    }

    /// Remove the directory `name` of `dir`, at `path`, empty or emptied,
    /// once `-i` consents, and only while the name still holds the node
    /// `stat` read.
    fn remove_directory(
        &mut self,
        dir: &R::Dir,
        name: &str,
        path: &str,
        stat: Result<FileStat, Errno>,
    ) -> Result<Removed, RmError> {
        if !self.confirmed(&format!("remove directory {}?", quote(path)))? {
            return Ok(Removed::Kept);
        }
        let unlinked =
            stat.and_then(|stat| dir.unlink(name, EMPTIED, Some(&NodeExpect::of_node(stat.id))));
        self.unlinked(unlinked, path, true)
    }

    /// The directory operand at `place`, at `path`, would not open or list for
    /// `errno`; one no directory holds is only reported.
    fn unreadable_at(
        &mut self,
        place: &Place<R::Dir>,
        path: &str,
        errno: Errno,
        held: Option<&R::Dir>,
    ) -> Result<Removed, RmError> {
        match place {
            Place::In { dir, name } => self.unreadable(dir, name, path, errno, held),
            Place::Unnamed => {
                self.carried.carry(unread(path, errno))?;
                Ok(Removed::Kept)
            }
        }
    }

    /// The directory `name` of `dir`, at `path`, would not open or list for
    /// `errno`. GNU still removes it when it is empty — unasked, so never
    /// under `-i` — and otherwise names why it could not be read; `held` is
    /// the directory when it opened, whose node alone the removal may take.
    fn unreadable(
        &mut self,
        dir: &R::Dir,
        name: &str,
        path: &str,
        errno: Errno,
        held: Option<&R::Dir>,
    ) -> Result<Removed, RmError> {
        if self.options.interactive != Interactive::Always {
            let removed = match held {
                Some(held) => held.stat().and_then(|stat| {
                    dir.unlink(name, EMPTIED, Some(&NodeExpect::of_node(stat.id)))
                }),
                None => dir.unlink(name, UnlinkFlags::DIRECTORY, None),
            };
            if removed.is_ok() {
                self.report(path, true)?;
                return Ok(Removed::Whole);
            }
        }
        self.carried.carry(unread(path, errno))?;
        Ok(Removed::Kept)
    }

    /// Report the unlink of `path`, a directory when `directory`, under `-v`,
    /// or carry its refusal past.
    fn unlinked(
        &mut self,
        unlinked: Result<(), Errno>,
        path: &str,
        directory: bool,
    ) -> Result<Removed, RmError> {
        let unlinked = unlinked.map_err(|errno| RmError::Remove {
            path: String::from(path),
            errno,
        });
        if self.carried.absorb(unlinked)?.is_none() {
            return Ok(Removed::Kept);
        }
        self.report(path, directory)?;
        Ok(Removed::Whole)
    }

    /// Note in the directory being walked whether an entry of it went.
    fn settle(&mut self, removed: Removed) {
        if removed == Removed::Kept {
            if let Some(whole) = self.whole.last_mut() {
                *whole = false;
            }
        }
    }

    /// Ask a per-object `-i` question; `Ok(true)` means proceed.
    fn confirmed(&self, question: &str) -> Result<bool, RmError> {
        if self.options.interactive != Interactive::Always {
            return Ok(true);
        }
        self.prompt.confirm(question).map_err(RmError::Prompt)
    }

    /// Report one removal under `-v`, in the GNU wording.
    fn report(&self, path: &str, directory: bool) -> Result<(), RmError> {
        if !self.options.verbose {
            return Ok(());
        }
        let line = if directory {
            format!("removed directory {path}\n", path = quote(path))
        } else {
            format!("removed {path}\n", path = quote(path))
        };
        self.out.write_all(line.as_bytes()).map_err(RmError::Output)
    }
}

impl<R: Roots> Visitor<R::Dir> for Removing<'_, R> {
    type Error = RmError;

    fn enter(&mut self, dir: &R::Dir, entry: &Entry, path: &str) -> Result<Step<R::Dir>, RmError> {
        if entry.kind != Kind::Directory {
            let link = entry.kind == Kind::Link;
            let removed =
                self.remove_file(dir, &entry.name, path, link, Known::Listed(entry.id))?;
            self.settle(removed);
            return Ok(Step::Skip);
        }
        if self.options.interactive == Interactive::Always {
            let removed = match dir.open_dir(&entry.name) {
                Err(errno) => {
                    self.carried.carry(unread(path, errno))?;
                    Removed::Kept
                }
                Ok(held) => match self.descent(&held, path)? {
                    Descent::Into => {
                        self.whole.push(true);
                        return Ok(Step::Into(held));
                    }
                    Descent::Empty => self.remove_directory(dir, &entry.name, path, held.stat())?,
                    Descent::Declined => Removed::Kept,
                },
            };
            self.settle(removed);
            return Ok(Step::Skip);
        }
        self.whole.push(true);
        Ok(Step::Descend)
    }

    fn leave(
        &mut self,
        dir: &R::Dir,
        entry: &Entry,
        path: &str,
        held: &R::Dir,
    ) -> Result<(), RmError> {
        let removed = if self.whole.pop() == Some(true) {
            self.remove_directory(dir, &entry.name, path, held.stat())?
        } else {
            Removed::Kept
        };
        self.settle(removed);
        Ok(())
    }

    fn refused(
        &mut self,
        dir: &R::Dir,
        entry: &Entry,
        path: &str,
        errno: Errno,
    ) -> Result<(), RmError> {
        self.whole.pop();
        let removed = self.unreadable(dir, &entry.name, path, errno, None)?;
        self.settle(removed);
        Ok(())
    }

    fn unlisted_root(&mut self, _path: &str, errno: Errno) -> Result<(), RmError> {
        self.unlisted = Some(errno);
        Ok(())
    }
}

/// How GNU names a node in an `-i` question.
const fn described(stat: &FileStat) -> &'static str {
    match stat.kind {
        FileKind::Regular if stat.size == 0 => "regular empty file",
        FileKind::Regular => "regular file",
        FileKind::Symlink => "symbolic link",
        FileKind::Directory => "directory",
    }
}

/// The directory `path` could not be read, for `errno`.
fn unread(path: &str, errno: Errno) -> RmError {
    RmError::Read {
        path: String::from(path),
        errno,
    }
}

/// Whether `path`'s last component is `.` or `..`, which POSIX never lets
/// `rm` remove.
fn names_dot(path: &str) -> bool {
    matches!(leaf_name(path), "." | "..")
}

#[cfg(test)]
mod tests {
    use super::{run as engine_run, USAGE};
    use crate::command::{Command, Interactive, Options};
    use crate::error::RmError;
    use crate::io::{Output, Prompt};
    use alloc::boxed::Box;
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;
    use core::cell::RefCell;
    use tairix_abi::Errno;
    use tairix_fscopy::fixture::Tree;
    use tairix_fscopy::tree::Kind;
    use tairix_help::{HelpSource, SourceError};

    /// A prompt no non-interactive run may ever reach.
    struct NeverAsked;

    impl Prompt for NeverAsked {
        fn confirm(&self, question: &str) -> Result<bool, Errno> {
            panic!("unexpected prompt: {question}");
        }

        fn at_terminal(&self) -> bool {
            false
        }
    }

    /// A scripted prompt: answers in order, recording every question; an
    /// exhausted script fails the read.
    struct Answers {
        replies: RefCell<Vec<bool>>,
        asked: RefCell<Vec<String>>,
        terminal: bool,
    }

    impl Answers {
        fn new(replies: &[bool]) -> Self {
            Self {
                replies: RefCell::new(replies.to_vec()),
                asked: RefCell::new(Vec::new()),
                terminal: false,
            }
        }

        /// The same, its replies coming from a terminal.
        fn at_a_terminal(replies: &[bool]) -> Self {
            Self {
                terminal: true,
                ..Self::new(replies)
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

        fn at_terminal(&self) -> bool {
            self.terminal
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

    /// A Help tree holding one canonical `rm.md` document.
    struct OneDoc;

    const DOC: &str = "## NAME\n\nrm — remove files and directories\n\n\
                       ## SYNOPSIS\n\n`rm [-r] [--] file...`\n\n\
                       ## DESCRIPTION\n\nRemoves things.\n";

    impl HelpSource for OneDoc {
        fn locale_dirs(&self) -> Result<Vec<String>, SourceError> {
            Ok(alloc::vec![String::from("en-US")])
        }

        fn read(&self, locale_dir: &str, file_name: &str) -> Result<Option<Vec<u8>>, SourceError> {
            if locale_dir == "en-US" && file_name == "rm.md" {
                Ok(Some(DOC.as_bytes().to_vec()))
            } else {
                Ok(None)
            }
        }
    }

    /// The engine under a prompt that must never be reached, answering
    /// how a failure that ends the run is shown.
    fn run(command: Command, tree: &Tree, out: &Recorder) -> Result<(), String> {
        match engine_run(
            command,
            None,
            tree,
            &NeverAsked,
            &NoHelp,
            out,
            &Recorder::new(),
        ) {
            Ok(true) => Ok(()),
            Ok(false) => Err(String::from("an operand failed")),
            Err(fatal) => Err(format!("rm: {fatal}\n")),
        }
    }

    /// The engine with its error stream captured: whether it ran clean, and
    /// what it reported. A failed operand is reported and the run goes on.
    fn run_reporting(command: Command, tree: &Tree) -> (bool, String) {
        let err = Recorder::new();
        let clean = engine_run(
            command,
            None,
            tree,
            &NeverAsked,
            &NoHelp,
            &Recorder::new(),
            &err,
        )
        .expect("no failure ends the run");
        (clean, err.text())
    }

    /// What the run unlinked and is gone, in the order it was unlinked.
    fn removed(tree: &Tree) -> Vec<String> {
        tree.calls()
            .iter()
            .filter_map(|call| call.strip_prefix("unlink "))
            .filter(|path| !tree.exists(path))
            .map(String::from)
            .collect()
    }

    /// Every name whose protection the run asked about, in order.
    fn asked(tree: &Tree) -> Vec<String> {
        tree.calls()
            .iter()
            .filter_map(|call| call.strip_prefix("access "))
            .map(String::from)
            .collect()
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

    fn remove_with(options: Options, paths: &[&str]) -> Command {
        Command::Remove {
            options,
            paths: paths.iter().map(|p| (*p).to_string()).collect::<Vec<_>>(),
        }
    }

    fn remove(recursive: bool, force: bool, paths: &[&str]) -> Command {
        remove_with(
            Options {
                recursive,
                force,
                ..Options::DEFAULT
            },
            paths,
        )
    }

    #[test]
    fn help_writes_usage() {
        let tree = Tree::new();
        let out = Recorder::new();
        assert_eq!(run(Command::Help, &tree, &out), Ok(()));
        assert_eq!(out.text(), USAGE);
    }

    #[test]
    fn help_renders_the_short_help_from_the_document() {
        let tree = Tree::new();
        let out = Recorder::new();
        assert_eq!(
            engine_run(
                Command::Help,
                None,
                &tree,
                &NeverAsked,
                &OneDoc,
                &out,
                &Recorder::new()
            ),
            Ok(true)
        );
        let text = out.text();
        assert!(text.contains("rm — remove files and directories"), "{text}");
        assert!(text.contains("rm [-r] [--] file..."), "{text}");
    }

    #[test]
    fn removes_a_single_file() {
        let tree = Tree::new().file("/a.txt", b"x");
        let out = Recorder::new();
        assert_eq!(run(remove(false, false, &["/a.txt"]), &tree, &out), Ok(()));
        assert_eq!(removed(&tree), ["/a.txt"]);
        // `rm` is silent on success.
        assert_eq!(out.text(), "");
    }

    #[test]
    fn removes_several_files_in_order() {
        let tree = Tree::new()
            .file("/a", b"x")
            .file("/b", b"x")
            .file("/c", b"x");
        let out = Recorder::new();
        assert_eq!(
            run(remove(false, false, &["/a", "/b", "/c"]), &tree, &out),
            Ok(())
        );
        assert_eq!(removed(&tree), ["/a", "/b", "/c"]);
    }

    #[test]
    fn directory_without_recursive_fails_closed() {
        let tree = Tree::new().dir("/d");
        assert_eq!(
            run_reporting(remove(false, false, &["/d"]), &tree),
            (
                false,
                format!("rm: cannot remove '/d': {}\n", Errno::IsADirectory)
            )
        );
        assert!(removed(&tree).is_empty());
        assert!(tree.exists("/d"));
    }

    #[test]
    fn recursive_removes_contents_before_the_directory() {
        // /d holds a file and a nested directory with its own file.
        let tree = Tree::new().file("/d/f", b"x").file("/d/sub/g", b"x");
        let out = Recorder::new();
        assert_eq!(run(remove(true, false, &["/d"]), &tree, &out), Ok(()));
        // Depth-first: every directory is removed after what it held.
        assert_eq!(removed(&tree), ["/d/f", "/d/sub/g", "/d/sub", "/d"]);
    }

    #[test]
    fn recursive_removes_an_empty_directory() {
        let tree = Tree::new().dir("/empty");
        let out = Recorder::new();
        assert_eq!(run(remove(true, false, &["/empty"]), &tree, &out), Ok(()));
        assert_eq!(removed(&tree), ["/empty"]);
    }

    #[test]
    fn missing_operand_fails_closed_without_force() {
        let tree = Tree::new();
        assert_eq!(
            run_reporting(remove(false, false, &["/absent"]), &tree),
            (
                false,
                format!("rm: cannot remove '/absent': {}\n", Errno::NotFound)
            )
        );
    }

    #[test]
    fn force_skips_a_missing_operand() {
        let tree = Tree::new().file("/present", b"x");
        let out = Recorder::new();
        // The missing operand is skipped; the present one is still removed.
        assert_eq!(
            run(remove(false, true, &["/absent", "/present"]), &tree, &out),
            Ok(())
        );
        assert_eq!(removed(&tree), ["/present"]);
    }

    #[test]
    fn force_does_not_mask_a_permission_error() {
        // `-f` suppresses only NotFound; a permission denial still surfaces.
        let tree = Tree::new()
            .file("/x", b"x")
            .refuse("open_node /x", Errno::PermissionDenied);
        assert_eq!(
            run_reporting(remove(false, true, &["/x"]), &tree),
            (
                false,
                format!("rm: cannot remove '/x': {}\n", Errno::PermissionDenied)
            )
        );
        assert!(tree.exists("/x"));
    }

    #[test]
    fn a_failed_operand_is_reported_and_the_next_still_removed() {
        // GNU 9.4: `rm missing a` names the missing one, removes `a` and
        // exits 1.
        let tree = Tree::new().file("/a", b"x").file("/b", b"x");
        assert_eq!(
            run_reporting(remove(false, false, &["/absent", "/a"]), &tree),
            (
                false,
                format!("rm: cannot remove '/absent': {}\n", Errno::NotFound)
            )
        );
        assert_eq!(removed(&tree), ["/a"]);
    }

    #[test]
    fn an_unreadable_directory_holding_something_is_reported_and_kept() {
        let tree = Tree::new()
            .file("/d/f", b"x")
            .refuse("entries /d", Errno::PermissionDenied);
        assert_eq!(
            run_reporting(remove(true, false, &["/d"]), &tree),
            (
                false,
                format!("rm: cannot remove '/d': {}\n", Errno::PermissionDenied)
            )
        );
        assert!(removed(&tree).is_empty());
        assert!(tree.exists("/d/f"));
    }

    #[test]
    fn an_unreadable_empty_directory_is_removed_as_gnu_removes_it() {
        let tree = Tree::new()
            .dir("/d/e")
            .refuse("entries /d/e", Errno::PermissionDenied);
        assert_eq!(
            run_reporting(remove(true, false, &["/d"]), &tree),
            (true, String::new())
        );
        assert_eq!(removed(&tree), ["/d/e", "/d"]);
    }

    #[test]
    fn a_failed_entry_is_reported_its_siblings_removed_and_its_ancestors_kept() {
        let tree = Tree::new()
            .file("/d/a", b"x")
            .file("/d/b", b"")
            .link("/d/s/x", "/nowhere")
            .refuse("unlink /d/a", Errno::PermissionDenied);
        assert_eq!(
            run_reporting(remove(true, false, &["/d"]), &tree),
            (
                false,
                format!("rm: cannot remove '/d/a': {}\n", Errno::PermissionDenied)
            )
        );
        assert_eq!(removed(&tree), ["/d/b", "/d/s/x", "/d/s"]);
        assert!(tree.exists("/d"));
    }

    #[test]
    fn a_link_is_removed_as_itself_and_never_descended() {
        let tree = Tree::new().file("/target/kept", b"x").link("/l", "/target");
        assert_eq!(
            run_reporting(remove(true, false, &["/l"]), &tree),
            (true, String::new())
        );
        assert_eq!(removed(&tree), ["/l"]);
        assert!(tree.exists("/target/kept"));
    }

    #[test]
    fn a_failed_unlink_surfaces_the_errno() {
        let tree = Tree::new()
            .file("/a", b"x")
            .refuse("unlink /a", Errno::PermissionDenied);
        assert_eq!(
            run_reporting(remove(false, false, &["/a"]), &tree),
            (
                false,
                format!("rm: cannot remove '/a': {}\n", Errno::PermissionDenied)
            )
        );
    }

    #[test]
    fn a_failed_directory_unlink_is_reported_after_its_contents_go() {
        // The child unlinks, then removing the directory itself fails.
        let tree = Tree::new()
            .file("/d/f", b"x")
            .refuse("unlink /d", Errno::PermissionDenied);
        assert_eq!(
            run_reporting(remove(true, false, &["/d"]), &tree),
            (
                false,
                format!("rm: cannot remove '/d': {}\n", Errno::PermissionDenied)
            )
        );
        assert_eq!(removed(&tree), ["/d/f"]);
    }

    #[test]
    fn empty_force_run_removes_nothing() {
        let tree = Tree::new();
        let out = Recorder::new();
        assert_eq!(run(remove(false, true, &[]), &tree, &out), Ok(()));
        assert!(tree.calls().is_empty());
    }

    #[test]
    fn help_output_failure_propagates() {
        let tree = Tree::new();
        let out = Recorder::failing();
        assert_eq!(
            run(Command::Help, &tree, &out),
            Err(format!("rm: terminal write failed: {}\n", Errno::NotFound))
        );
    }

    #[test]
    fn no_preserve_root_lets_a_recursive_removal_empty_the_root() {
        // A child of the root is named with one slash, never `//f`.
        let tree = Tree::new().file("/f", b"x");
        let out = Recorder::new();
        let options = Options {
            recursive: true,
            verbose: true,
            preserve_root: false,
            ..Options::DEFAULT
        };
        engine_run(
            remove_with(options, &["/"]),
            None,
            &tree,
            &NeverAsked,
            &NoHelp,
            &out,
            &Recorder::new(),
        )
        .expect("no failure ends the run");
        assert!(!tree.exists("/f"));
        assert!(out.text().starts_with("removed '/f'\n"), "{}", out.text());
    }

    /// The root volume's own spelling.
    const ROOT_VOLUME: &str = "id::b7f2e4e6-8d7a-4ef8-a13e-d3b84d4e8001";

    #[test]
    fn preserve_root_refuses_the_root_however_spelled() {
        let tree = Tree::new().file("/f", b"x").named_root(ROOT_VOLUME, "/");
        let by_volume = format!("{ROOT_VOLUME}/");
        for spelled in ["//", "///", ROOT_VOLUME, by_volume.as_str()] {
            assert_eq!(
                run_reporting(remove(true, false, &[spelled]), &tree),
                (
                    false,
                    format!(
                        "rm: it is dangerous to operate recursively on '{spelled}' (same as '/')\n\
                         rm: use --no-preserve-root to override this failsafe\n"
                    )
                ),
                "{spelled}"
            );
        }
        assert!(tree.exists("/f"));
    }

    #[test]
    fn a_root_other_than_the_view_root_is_emptied_but_stays() {
        let tree = Tree::new()
            .file("/home/f", b"x")
            .file("/kept", b"k")
            .named_root("Home:/", "/home");
        assert_eq!(
            run_reporting(remove(true, false, &["Home:/"]), &tree),
            (
                false,
                format!("rm: cannot remove 'Home:/': {}\n", Errno::Busy)
            )
        );
        assert!(!tree.exists("/home/f"));
        assert!(tree.exists("/home") && tree.exists("/kept"));
    }

    #[test]
    fn an_operand_naming_dot_or_nothing_is_described_as_gnu_does() {
        let tree = Tree::new().dir("/d");
        for spelled in [".", "/d/..", "d/."] {
            assert_eq!(
                run_reporting(remove(false, false, &[spelled]), &tree),
                (
                    false,
                    format!("rm: cannot remove '{spelled}': {}\n", Errno::IsADirectory)
                ),
                "{spelled}"
            );
        }
        assert_eq!(
            run_reporting(remove(false, false, &[""]), &tree),
            (
                false,
                format!("rm: cannot remove '': {}\n", Errno::NotFound)
            )
        );
        assert_eq!(
            run_reporting(remove(false, true, &[""]), &tree),
            (true, String::new())
        );
        assert!(tree.exists("/d"));
    }

    #[test]
    fn a_name_swapped_after_its_question_is_kept() {
        let tree = Tree::new().file("/f", b"agreed").file("/other", b"o");
        tree.before_line(
            "unlink /f",
            Box::new(|tree: &Tree| {
                tree.remove("/f");
                tree.write("/f", b"swapped in");
            }),
        );
        let err = Recorder::new();
        let interactive = Options {
            interactive: Interactive::Always,
            ..Options::DEFAULT
        };
        assert_eq!(
            engine_run(
                remove_with(interactive, &["/f"]),
                None,
                &tree,
                &Answers::new(&[true]),
                &NoHelp,
                &Recorder::new(),
                &err,
            ),
            Ok(false)
        );
        assert_eq!(
            err.text(),
            format!("rm: cannot remove '/f': {}\n", Errno::Stale)
        );
        assert_eq!(tree.bytes("/f"), Some(b"swapped in".to_vec()));
    }

    #[test]
    fn an_entry_swapped_during_a_walk_is_kept() {
        let tree = Tree::new().file("/d/a", b"a").file("/d/b", b"b");
        tree.before_line(
            "unlink /d/b",
            Box::new(|tree: &Tree| {
                tree.remove("/d/b");
                tree.write("/d/b", b"new");
            }),
        );
        assert_eq!(
            run_reporting(remove(true, false, &["/d"]), &tree),
            (
                false,
                format!("rm: cannot remove '/d/b': {}\n", Errno::Stale)
            )
        );
        assert!(!tree.exists("/d/a"));
        assert_eq!(tree.bytes("/d/b"), Some(b"new".to_vec()));
    }

    #[test]
    fn a_link_to_a_directory_is_removed_as_itself() {
        let tree = Tree::new().file("/d/kept", b"k").link("/l", "/d");
        assert_eq!(
            run_reporting(remove(false, false, &["/l"]), &tree),
            (true, String::new())
        );
        assert!(!tree.exists("/l"));
        assert!(tree.exists("/d/kept"));
        let tree = Tree::new().file("/d/kept", b"k").link("/l", "/d");
        assert_eq!(
            run_reporting(remove(true, false, &["/l"]), &tree),
            (true, String::new())
        );
        assert!(!tree.exists("/l"));
        assert!(tree.exists("/d/kept"), "a link operand is never descended");
    }

    #[test]
    fn without_recursive_the_root_is_a_directory_like_any_other() {
        let tree = Tree::new();
        assert_eq!(
            run_reporting(remove(false, false, &["/"]), &tree),
            (
                false,
                format!("rm: cannot remove '/': {}\n", Errno::IsADirectory)
            )
        );
    }

    #[test]
    fn preserve_root_refuses_the_root_operand() {
        let tree = Tree::new().file("/f", b"x");
        assert_eq!(
            run_reporting(remove(true, false, &["/"]), &tree),
            (
                false,
                String::from(
                    "rm: it is dangerous to operate recursively on '/'\n\
                     rm: use --no-preserve-root to override this failsafe\n"
                )
            )
        );
        assert!(tree.exists("/f"));
        assert!(tree.calls().is_empty(), "refused before any lookup");
    }

    /// POSIX forbids removing `.` or `..`: refused in GNU's words before
    /// anything is looked up, `-f` notwithstanding, and the tree untouched.
    #[test]
    fn a_dot_or_dot_dot_operand_is_refused_before_anything_is_looked_up() {
        let tree = Tree::new().file("/d/f", b"x");
        assert_eq!(
            run_reporting(remove(true, true, &["/d/.", "/d/.."]), &tree),
            (
                false,
                String::from(
                    "rm: refusing to remove '.' or '..' directory: skipping '/d/.'\n\
                     rm: refusing to remove '.' or '..' directory: skipping '/d/..'\n"
                )
            )
        );
        let dir = Options {
            dir: true,
            ..Options::DEFAULT
        };
        assert_eq!(
            run_reporting(remove_with(dir, &["/d/./"]), &tree),
            (
                false,
                String::from("rm: refusing to remove '.' or '..' directory: skipping '/d/./'\n")
            )
        );
        assert!(tree.calls().is_empty(), "{:?}", tree.calls());
        assert!(tree.exists("/d/f"));
    }

    #[test]
    fn dir_option_removes_an_empty_directory_without_recursive() {
        let tree = Tree::new().dir("/empty");
        let out = Recorder::new();
        assert_eq!(
            run(
                remove_with(
                    Options {
                        dir: true,
                        ..Options::DEFAULT
                    },
                    &["/empty"],
                ),
                &tree,
                &out,
            ),
            Ok(())
        );
        assert_eq!(removed(&tree), ["/empty"]);
    }

    #[test]
    fn dir_option_surfaces_the_filesystems_refusal_of_a_full_directory() {
        let tree = Tree::new().file("/full/f", b"x");
        assert_eq!(
            run_reporting(
                remove_with(
                    Options {
                        dir: true,
                        ..Options::DEFAULT
                    },
                    &["/full"],
                ),
                &tree,
            ),
            (
                false,
                format!("rm: cannot remove '/full': {}\n", Errno::NotEmpty)
            )
        );
    }

    #[test]
    fn verbose_reports_each_removal_in_gnu_wording() {
        let tree = Tree::new().file("/d/f", b"x");
        let out = Recorder::new();
        assert_eq!(
            run(
                remove_with(
                    Options {
                        recursive: true,
                        verbose: true,
                        ..Options::DEFAULT
                    },
                    &["/d"],
                ),
                &tree,
                &out,
            ),
            Ok(())
        );
        assert_eq!(out.text(), "removed '/d/f'\nremoved directory '/d'\n");
    }

    #[test]
    fn interactive_always_asks_before_each_removal() {
        let tree = Tree::new().file("/a", b"x").file("/b", b"x");
        let out = Recorder::new();
        let prompt = Answers::new(&[true, false]);
        // `/a` is confirmed and removed; `/b` is declined and skipped, and
        // the run still succeeds.
        assert_eq!(
            engine_run(
                remove_with(
                    Options {
                        interactive: Interactive::Always,
                        ..Options::DEFAULT
                    },
                    &["/a", "/b"],
                ),
                None,
                &tree,
                &prompt,
                &NoHelp,
                &out,
                &Recorder::new(),
            ),
            Ok(true)
        );
        assert_eq!(removed(&tree), ["/a"]);
        assert_eq!(
            prompt.asked(),
            ["remove regular file '/a'?", "remove regular file '/b'?"]
        );
    }

    /// A name's control bytes are escaped where GNU escapes them — the
    /// question and the `-v` report alike — never written raw.
    #[test]
    fn a_name_holding_a_control_byte_is_escaped_everywhere_it_is_named() {
        let tree = Tree::new().file("/a\u{1b}[2J", b"x");
        let out = Recorder::new();
        let prompt = Answers::new(&[true]);
        assert_eq!(
            engine_run(
                remove_with(
                    Options {
                        interactive: Interactive::Always,
                        verbose: true,
                        ..Options::DEFAULT
                    },
                    &["/a\u{1b}[2J"],
                ),
                None,
                &tree,
                &prompt,
                &NoHelp,
                &out,
                &Recorder::new(),
            ),
            Ok(true)
        );
        assert_eq!(prompt.asked(), ["remove regular file '/a'$'\\033''[2J'?"]);
        assert_eq!(out.text(), "removed '/a'$'\\033''[2J'\n");
    }

    #[test]
    fn interactive_names_each_kind_as_gnu_does() {
        let tree = Tree::new()
            .file("/e", b"")
            .file("/f", b"x")
            .link("/l", "/f")
            .dir("/d");
        let prompt = Answers::new(&[false, false, false, false]);
        let interactive = Options {
            interactive: Interactive::Always,
            dir: true,
            ..Options::DEFAULT
        };
        assert_eq!(
            engine_run(
                remove_with(interactive, &["/e", "/f", "/l", "/d"]),
                None,
                &tree,
                &prompt,
                &NoHelp,
                &Recorder::new(),
                &Recorder::new(),
            ),
            Ok(true)
        );
        assert_eq!(
            prompt.asked(),
            [
                "remove regular empty file '/e'?",
                "remove regular file '/f'?",
                "remove symbolic link '/l'?",
                "remove directory '/d'?",
            ]
        );
    }

    #[test]
    fn interactive_asks_only_to_remove_an_empty_directory() {
        let tree = Tree::new().dir("/e");
        let prompt = Answers::new(&[true]);
        let interactive = Options {
            recursive: true,
            interactive: Interactive::Always,
            ..Options::DEFAULT
        };
        assert_eq!(
            engine_run(
                remove_with(interactive, &["/e"]),
                None,
                &tree,
                &prompt,
                &NoHelp,
                &Recorder::new(),
                &Recorder::new(),
            ),
            Ok(true)
        );
        assert_eq!(prompt.asked(), ["remove directory '/e'?"]);
        assert_eq!(removed(&tree), ["/e"]);
    }

    #[test]
    fn a_declined_entry_keeps_its_ancestors_without_asking() {
        let tree = Tree::new().file("/d/f", b"x").file("/d/g", b"x");
        let prompt = Answers::new(&[true, false, true]);
        let interactive = Options {
            recursive: true,
            interactive: Interactive::Always,
            ..Options::DEFAULT
        };
        assert_eq!(
            engine_run(
                remove_with(interactive, &["/d"]),
                None,
                &tree,
                &prompt,
                &NoHelp,
                &Recorder::new(),
                &Recorder::new(),
            ),
            Ok(true),
            "a declined question is no failure"
        );
        assert_eq!(
            prompt.asked(),
            [
                "descend into directory '/d'?",
                "remove regular file '/d/f'?",
                "remove regular file '/d/g'?",
            ]
        );
        assert_eq!(removed(&tree), ["/d/g"]);
    }

    #[test]
    fn interactive_always_declining_the_descent_skips_the_directory() {
        let tree = Tree::new().file("/d/f", b"x");
        let out = Recorder::new();
        let prompt = Answers::new(&[false]);
        assert_eq!(
            engine_run(
                remove_with(
                    Options {
                        recursive: true,
                        interactive: Interactive::Always,
                        ..Options::DEFAULT
                    },
                    &["/d"],
                ),
                None,
                &tree,
                &prompt,
                &NoHelp,
                &out,
                &Recorder::new(),
            ),
            Ok(true)
        );
        assert!(removed(&tree).is_empty());
        assert_eq!(prompt.asked(), ["descend into directory '/d'?"]);
    }

    #[test]
    fn interactive_once_asks_once_for_many_operands() {
        let tree = Tree::new()
            .file("/a", b"x")
            .file("/b", b"x")
            .file("/c", b"x")
            .file("/d", b"x");
        let out = Recorder::new();
        let declined = Answers::new(&[false]);
        assert_eq!(
            engine_run(
                remove_with(
                    Options {
                        interactive: Interactive::Once,
                        ..Options::DEFAULT
                    },
                    &["/a", "/b", "/c", "/d"],
                ),
                None,
                &tree,
                &declined,
                &NoHelp,
                &out,
                &Recorder::new(),
            ),
            Ok(true)
        );
        assert!(removed(&tree).is_empty());
        assert_eq!(declined.asked(), ["remove 4 arguments?"]);

        let accepted = Answers::new(&[true]);
        assert_eq!(
            engine_run(
                remove_with(
                    Options {
                        interactive: Interactive::Once,
                        ..Options::DEFAULT
                    },
                    &["/a", "/b", "/c", "/d"],
                ),
                None,
                &tree,
                &accepted,
                &NoHelp,
                &out,
                &Recorder::new(),
            ),
            Ok(true)
        );
        assert_eq!(removed(&tree), ["/a", "/b", "/c", "/d"]);
    }

    #[test]
    fn interactive_once_asks_for_a_recursive_removal_and_not_for_few_files() {
        let tree = Tree::new().dir("/d");
        let out = Recorder::new();
        let prompt = Answers::new(&[true]);
        assert_eq!(
            engine_run(
                remove_with(
                    Options {
                        recursive: true,
                        interactive: Interactive::Once,
                        ..Options::DEFAULT
                    },
                    &["/d"],
                ),
                None,
                &tree,
                &prompt,
                &NoHelp,
                &out,
                &Recorder::new(),
            ),
            Ok(true)
        );
        assert_eq!(prompt.asked(), ["remove 1 argument recursively?"]);
        assert_eq!(removed(&tree), ["/d"]);

        // Three or fewer plain files ask nothing.
        let tree = Tree::new().file("/a", b"x");
        assert_eq!(
            run(
                remove_with(
                    Options {
                        interactive: Interactive::Once,
                        ..Options::DEFAULT
                    },
                    &["/a"],
                ),
                &tree,
                &out,
            ),
            Ok(())
        );
        assert_eq!(removed(&tree), ["/a"]);
    }

    /// GNU 9.4 at a terminal: `rm: remove write-protected regular empty file
    /// 'ro'?`, the file kept on a decline and removed on a `y`.
    #[test]
    fn a_write_protected_file_is_asked_about_at_a_terminal() {
        let tree = Tree::new()
            .file("/ro", b"")
            .file("/rf", b"x")
            .file("/w", b"x")
            .refuse("access /ro", Errno::PermissionDenied)
            .refuse("access /rf", Errno::PermissionDenied);
        let prompt = Answers::at_a_terminal(&[false, true]);
        assert_eq!(
            engine_run(
                remove(false, false, &["/ro", "/rf", "/w"]),
                None,
                &tree,
                &prompt,
                &NoHelp,
                &Recorder::new(),
                &Recorder::new(),
            ),
            Ok(true),
            "a declined question is no failure"
        );
        assert_eq!(
            prompt.asked(),
            [
                "remove write-protected regular empty file '/ro'?",
                "remove write-protected regular file '/rf'?",
            ]
        );
        assert_eq!(removed(&tree), ["/rf", "/w"]);
    }

    /// Under `-f`, and with no terminal to ask at, nothing is asked and the
    /// protection is not even read.
    #[test]
    fn a_write_protected_file_is_not_asked_about_under_force_or_without_a_terminal() {
        let protected = || {
            Tree::new()
                .file("/ro", b"x")
                .refuse("access /ro", Errno::PermissionDenied)
        };
        let tree = protected();
        let terminal = Answers::at_a_terminal(&[]);
        assert_eq!(
            engine_run(
                remove(false, true, &["/ro"]),
                None,
                &tree,
                &terminal,
                &NoHelp,
                &Recorder::new(),
                &Recorder::new(),
            ),
            Ok(true)
        );
        assert_eq!(removed(&tree), ["/ro"]);
        assert!(asked(&tree).is_empty());

        let tree = protected();
        assert_eq!(
            run(remove(false, false, &["/ro"]), &tree, &Recorder::new()),
            Ok(())
        );
        assert_eq!(removed(&tree), ["/ro"]);
        assert!(asked(&tree).is_empty());
    }

    /// A link is removed as itself, and never asked about as write-protected,
    /// nor is a directory.
    #[test]
    fn neither_a_link_nor_a_directory_is_asked_about_as_write_protected() {
        let tree = Tree::new()
            .link("/l", "/nowhere")
            .dir("/d")
            .refuse("access /l", Errno::PermissionDenied)
            .refuse("access /d", Errno::PermissionDenied);
        let prompt = Answers::at_a_terminal(&[]);
        let options = Options {
            dir: true,
            ..Options::DEFAULT
        };
        assert_eq!(
            engine_run(
                remove_with(options, &["/l", "/d"]),
                None,
                &tree,
                &prompt,
                &NoHelp,
                &Recorder::new(),
                &Recorder::new(),
            ),
            Ok(true)
        );
        assert!(prompt.asked().is_empty());
        assert!(asked(&tree).is_empty());
        assert_eq!(removed(&tree), ["/l", "/d"]);
    }

    /// `-i` names the protection in the question it asks anyway, terminal or
    /// not, and a file inside a tree is asked about as an operand is.
    #[test]
    fn interactive_names_a_write_protected_file_so_inside_a_tree_too() {
        let tree = Tree::new()
            .file("/d/f", b"x")
            .refuse("access /d/f", Errno::PermissionDenied);
        let prompt = Answers::new(&[true, false]);
        let interactive = Options {
            recursive: true,
            interactive: Interactive::Always,
            ..Options::DEFAULT
        };
        assert_eq!(
            engine_run(
                remove_with(interactive, &["/d"]),
                None,
                &tree,
                &prompt,
                &NoHelp,
                &Recorder::new(),
                &Recorder::new(),
            ),
            Ok(true)
        );
        assert_eq!(
            prompt.asked(),
            [
                "descend into directory '/d'?",
                "remove write-protected regular file '/d/f'?",
            ]
        );
        assert!(removed(&tree).is_empty());
    }

    /// A protection that cannot be read keeps the file and says why: it is
    /// never removed unasked on a guess.
    #[test]
    fn a_file_whose_protection_cannot_be_read_is_kept_and_reported() {
        let tree = Tree::new()
            .file("/x", b"x")
            .file("/y", b"x")
            .refuse("access /x", Errno::Stale);
        let err = Recorder::new();
        assert_eq!(
            engine_run(
                remove(false, false, &["/x", "/y"]),
                None,
                &tree,
                &Answers::at_a_terminal(&[]),
                &NoHelp,
                &Recorder::new(),
                &err,
            ),
            Ok(false)
        );
        assert_eq!(
            err.text(),
            format!("rm: cannot remove '/x': {}\n", Errno::Stale)
        );
        assert_eq!(removed(&tree), ["/y"]);
    }

    #[test]
    fn an_unanswerable_prompt_fails_closed() {
        let tree = Tree::new().file("/a", b"x");
        let out = Recorder::new();
        let prompt = Answers::new(&[]);
        assert_eq!(
            engine_run(
                remove_with(
                    Options {
                        interactive: Interactive::Always,
                        ..Options::DEFAULT
                    },
                    &["/a"],
                ),
                None,
                &tree,
                &prompt,
                &NoHelp,
                &out,
                &Recorder::new(),
            ),
            Err(RmError::Prompt(Errno::NotFound))
        );
        assert!(removed(&tree).is_empty());
    }

    /// `/d/sub` holding a file, and beside the tree a victim with a link to
    /// it waiting at `/planted`.
    fn beside_a_victim() -> Tree {
        Tree::new()
            .file("/d/sub/f", b"mine")
            .file("/victim/precious", b"theirs")
            .link("/planted", "/victim")
    }

    /// Swap `/d/sub`, while it is still the directory, for the link to the
    /// victim.
    fn swap_sub_for_the_link(tree: &Tree) {
        if tree.kind("/d/sub") == Some(Kind::Directory) {
            tree.rename("/d/sub", "/aside");
            tree.rename("/planted", "/d/sub");
        }
    }

    /// Wherever among its seam calls the swap lands, the walk never descends
    /// the link and never touches what it names.
    #[test]
    fn a_subdirectory_swapped_for_a_link_mid_walk_never_reaches_its_target() {
        let undisturbed = beside_a_victim();
        run_reporting(remove(true, false, &["/d"]), &undisturbed);
        for call in 0..undisturbed.calls().len() {
            let tree = beside_a_victim();
            tree.before_call(call, Box::new(swap_sub_for_the_link));
            run_reporting(remove(true, false, &["/d"]), &tree);
            assert_eq!(
                tree.bytes("/victim/precious").as_deref(),
                Some(&b"theirs"[..]),
                "the swap before call {call}"
            );
        }
    }

    /// The directory the walk emptied is the only one its removal may take:
    /// one renamed into its name meanwhile stays, and the refusal is named.
    #[test]
    fn a_directory_replaced_once_emptied_is_kept_and_reported() {
        let tree = Tree::new().file("/d/sub/f", b"x").dir("/other");
        tree.before_line(
            "unlink /d/sub",
            Box::new(|tree: &Tree| {
                tree.rename("/d/sub", "/aside");
                tree.rename("/other", "/d/sub");
            }),
        );
        assert_eq!(
            run_reporting(remove(true, false, &["/d"]), &tree),
            (
                false,
                format!("rm: cannot remove '/d/sub': {}\n", Errno::Stale)
            )
        );
        assert_eq!(tree.kind("/d/sub"), Some(Kind::Directory));
        assert_eq!(tree.kind("/d"), Some(Kind::Directory));
        assert_eq!(removed(&tree), ["/d/sub/f"]);
    }
}
