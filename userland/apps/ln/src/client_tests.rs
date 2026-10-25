extern crate std;

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::RefCell;

use tairix_abi::{Errno, FileId, FileKind, RealpathMode};
use tairix_help::{HelpSource, SourceError};

use super::{relative_spelling, run, USAGE};
use crate::command::parse;
use crate::error::LnError;
use crate::io::{FileSystem, FinalLink, Node, Output, Prompt};

/// One name in the tree: the node it is, and the node a final link through it
/// reaches (itself, unless it is a symbolic link; [`None`] when one dangles).
#[derive(Clone, Debug)]
struct Name {
    path: String,
    own: Node,
    named: Option<Node>,
}

/// What a fixture name is made as.
#[derive(Clone, Copy)]
enum Made {
    File,
    Dir,
    /// A symbolic link to a regular file.
    Link,
    /// A symbolic link to a directory.
    LinkToDir,
    /// A symbolic link that names nothing.
    Dangling,
}

/// The working directory a relative fixture path is in.
const WORKING: &str = "/w";

/// `path` as the one absolute spelling the fixture keeps a name under: in the
/// working directory when relative, its `.` and `..` resolved lexically.
fn normal(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    let rooted = if path.starts_with('/') {
        String::from(path)
    } else {
        alloc::format!("{WORKING}/{path}")
    };
    for part in rooted.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            name => parts.push(name),
        }
    }
    alloc::format!("/{}", parts.join("/"))
}

/// An in-memory tree of names and node identities, recording the links made
/// and the names a link replaced.
#[derive(Default)]
struct TreeFs {
    names: RefCell<Vec<Name>>,
    next_node: RefCell<u64>,
    /// `(link, target)` per symbolic link made.
    created: RefCell<Vec<(String, String)>>,
    /// `(link, target, -L asked for)` per hard link made.
    hard_linked: RefCell<Vec<(String, String, bool)>>,
    /// The names a new link took the place of.
    replaced: RefCell<Vec<String>>,
    symlink_errno: Option<Errno>,
    link_errno: Option<Errno>,
    /// Scripted canonical answers, `(path, canonical)`; a path with no entry
    /// canonicalises to its [`normal`] spelling.
    canonical: RefCell<Vec<(String, String)>>,
    canonicalize_errno: Option<Errno>,
}

impl TreeFs {
    fn with(names: &[(&str, Made)]) -> Self {
        let fs = Self::default();
        for &(path, made) in names {
            fs.add(path, made);
        }
        fs
    }

    /// The fixture with `pairs` as its scripted canonicalisation.
    fn canonicalising(self, pairs: &[(&str, &str)]) -> Self {
        *self.canonical.borrow_mut() = pairs
            .iter()
            .map(|(p, c)| (String::from(*p), String::from(*c)))
            .collect();
        self
    }

    fn refusing(self, errno: Errno) -> Self {
        Self {
            symlink_errno: Some(errno),
            link_errno: Some(errno),
            ..self
        }
    }

    fn fresh(&self, kind: FileKind) -> Node {
        let mut next = self.next_node.borrow_mut();
        *next += 1;
        Node {
            kind,
            id: FileId {
                volume: [7; 16],
                node: *next,
            },
            links: 1,
        }
    }

    fn add(&self, path: &str, made: Made) {
        let (own, named) = match made {
            Made::File => {
                let node = self.fresh(FileKind::Regular);
                (node, Some(node))
            }
            Made::Dir => {
                let node = self.fresh(FileKind::Directory);
                (node, Some(node))
            }
            Made::Link => (
                self.fresh(FileKind::Symlink),
                Some(self.fresh(FileKind::Regular)),
            ),
            Made::LinkToDir => (
                self.fresh(FileKind::Symlink),
                Some(self.fresh(FileKind::Directory)),
            ),
            Made::Dangling => (self.fresh(FileKind::Symlink), None),
        };
        self.put(path, own, named);
    }

    /// `path` as a second name for the node `of` is.
    fn second_name(self, path: &str, of: &str) -> Self {
        let node = self.own_of(of);
        self.put(path, node, Some(node));
        self
    }

    /// `path` as a symbolic link reaching the node `of` is.
    fn link_to(self, path: &str, of: &str) -> Self {
        let link = self.fresh(FileKind::Symlink);
        let named = self.own_of(of);
        self.put(path, link, Some(named));
        self
    }

    fn own_of(&self, path: &str) -> Node {
        self.find(path)
            .map(|name| name.own)
            .expect("a fixture name")
    }

    fn find(&self, path: &str) -> Option<Name> {
        let path = normal(path);
        self.names
            .borrow()
            .iter()
            .find(|name| name.path == path)
            .cloned()
    }

    fn put(&self, path: &str, own: Node, named: Option<Node>) {
        let path = normal(path);
        let mut all = self.names.borrow_mut();
        all.retain(|name| name.path != path);
        all.push(Name { path, own, named });
    }

    fn holds(&self, path: &str) -> bool {
        self.find(path).is_some()
    }

    fn created(&self) -> Vec<(String, String)> {
        self.created.borrow().clone()
    }

    fn hard_linked(&self) -> Vec<(String, String, bool)> {
        self.hard_linked.borrow().clone()
    }

    fn replaced(&self) -> Vec<String> {
        self.replaced.borrow().clone()
    }

    /// A name is taken: refused unless it is to be replaced, recorded if so.
    fn take(&self, link: &str, replace: bool) -> Result<(), Errno> {
        if self.holds(link) {
            if !replace {
                return Err(Errno::AlreadyExists);
            }
            self.replaced.borrow_mut().push(String::from(link));
        }
        Ok(())
    }
}

impl FileSystem for TreeFs {
    fn node(&self, path: &str, link: FinalLink) -> Result<Option<Node>, Errno> {
        let Some(name) = self.find(path) else {
            return Ok(None);
        };
        let node = match link {
            FinalLink::Keep => Some(name.own),
            FinalLink::Follow => name.named,
        };
        // Every name of a node counts towards its links.
        let names = self.names.borrow();
        Ok(node.map(|node| Node {
            links: u32::try_from(names.iter().filter(|n| n.own.id == node.id).count())
                .unwrap_or(u32::MAX)
                .max(1),
            ..node
        }))
    }

    /// The canonicalisation the kernel would perform: a scripted mapping, or
    /// the path's [`normal`] spelling. `ln` never resolves anything itself,
    /// so this is the only source of a canonical answer.
    fn canonicalize(&self, path: &str, _mode: RealpathMode) -> Result<String, Errno> {
        if let Some(errno) = self.canonicalize_errno {
            return Err(errno);
        }
        Ok(self
            .canonical
            .borrow()
            .iter()
            .find(|(p, _)| p == path)
            .map_or_else(|| normal(path), |(_, c)| c.clone()))
    }

    fn symlink(&self, target: &str, link: &str, replace: bool) -> Result<(), Errno> {
        if let Some(errno) = self.symlink_errno {
            return Err(errno);
        }
        self.take(link, replace)?;
        self.add(link, Made::Dangling);
        self.created
            .borrow_mut()
            .push((String::from(link), String::from(target)));
        Ok(())
    }

    fn link(
        &self,
        target: &str,
        link: &str,
        dereference: bool,
        replace: bool,
    ) -> Result<(), Errno> {
        if let Some(errno) = self.link_errno {
            return Err(errno);
        }
        let follow = if dereference {
            FinalLink::Follow
        } else {
            FinalLink::Keep
        };
        let node = self.node(target, follow)?.ok_or(Errno::NotFound)?;
        // A second name for a directory is refused whatever `-d` said.
        if node.kind == FileKind::Directory {
            return Err(Errno::IsADirectory);
        }
        self.take(link, replace)?;
        self.put(link, node, Some(node));
        // The posture is recorded so a test can prove `-L`/`-P` reach the
        // seam rather than being parsed and dropped.
        self.hard_linked
            .borrow_mut()
            .push((String::from(link), String::from(target), dereference));
        Ok(())
    }
}

/// Records the rendered output, and answers `-i` with a fixed reply.
#[derive(Default)]
struct Recorder {
    text: RefCell<String>,
    answer: bool,
    asked: RefCell<Vec<String>>,
}

impl Recorder {
    fn answering(answer: bool) -> Self {
        Self {
            answer,
            ..Self::default()
        }
    }

    fn text(&self) -> String {
        self.text.borrow().clone()
    }
}

impl Output for Recorder {
    fn write_all(&self, bytes: &[u8]) -> Result<(), Errno> {
        self.text
            .borrow_mut()
            .push_str(core::str::from_utf8(bytes).unwrap_or_default());
        Ok(())
    }
}

impl Prompt for Recorder {
    fn confirm(&self, question: &str) -> Result<bool, Errno> {
        self.asked.borrow_mut().push(question.to_string());
        Ok(self.answer)
    }
}

/// A help source with no documents, so the short help falls back to the usage
/// banner.
struct NoHelp;

impl HelpSource for NoHelp {
    fn locale_dirs(&self) -> Result<Vec<String>, SourceError> {
        Ok(Vec::new())
    }

    fn read(&self, _locale_dir: &str, _file_name: &str) -> Result<Option<Vec<u8>>, SourceError> {
        Ok(None)
    }
}

/// Parse and run `args`, answering what the tool reported when it did not run
/// clean: a failed link is reported and the next still made.
fn go(args: &[&str], fs: &TreeFs, out: &Recorder) -> Result<(), String> {
    let command = parse(args).map_err(|refused| alloc::format!("ln: {refused}\n"))?;
    let err = Recorder::default();
    match run(command, None, fs, out, &NoHelp, out, &err) {
        Ok(true) => Ok(()),
        Ok(false) => Err(err.text()),
        Err(fatal) => Err(alloc::format!("ln: {fatal}\n")),
    }
}

/// `failure` as the tool reports it.
fn shown(failure: &LnError) -> Result<(), String> {
    Err(alloc::format!("ln: {failure}\n"))
}

/// A link that could not be made.
fn refused(link: &str, target: &str, symbolic: bool, errno: Errno) -> LnError {
    LnError::Create {
        link: String::from(link),
        target: String::from(target),
        symbolic,
        errno,
    }
}

fn same_file(target: &str, link: &str) -> LnError {
    LnError::SameFile {
        target: String::from(target),
        link: String::from(link),
    }
}

#[test]
fn a_symbolic_link_is_created_at_the_named_destination() {
    let fs = TreeFs::default();
    let out = Recorder::default();
    assert_eq!(go(&["-s", "/a/target", "/b/link"], &fs, &out), Ok(()));
    assert_eq!(
        fs.created(),
        [(String::from("/b/link"), String::from("/a/target"))]
    );
    // Silent on success without `-v`.
    assert_eq!(out.text(), "");
}

#[test]
fn the_target_is_stored_verbatim_and_may_dangle() {
    let fs = TreeFs::default();
    let out = Recorder::default();
    // A relative target carrying `..`, naming nothing that exists: the tool
    // stores the spelling and never resolves it.
    assert_eq!(go(&["-s", "../elsewhere/x", "/b/link"], &fs, &out), Ok(()));
    assert_eq!(
        fs.created(),
        [(String::from("/b/link"), String::from("../elsewhere/x"))]
    );
}

/// GNU names the one-operand form's link `./name`.
#[test]
fn one_operand_links_under_the_targets_own_name_here() {
    let fs = TreeFs::default();
    let out = Recorder::default();
    assert_eq!(go(&["-sv", "/a/b/target.txt"], &fs, &out), Ok(()));
    assert_eq!(
        fs.created(),
        [(
            String::from("./target.txt"),
            String::from("/a/b/target.txt")
        )]
    );
    assert_eq!(out.text(), "'./target.txt' -> '/a/b/target.txt'\n");
}

#[test]
fn a_directory_destination_receives_the_links() {
    let fs = TreeFs::with(&[("/dir", Made::Dir)]);
    let out = Recorder::default();
    assert_eq!(go(&["-s", "/a/one", "/b/two", "/dir"], &fs, &out), Ok(()));
    assert_eq!(
        fs.created(),
        [
            (String::from("/dir/one"), String::from("/a/one")),
            (String::from("/dir/two"), String::from("/b/two")),
        ]
    );
}

#[test]
fn a_link_to_a_directory_receives_the_links_unless_n_is_given() {
    let fs = TreeFs::with(&[("/dir", Made::LinkToDir)]);
    let out = Recorder::default();
    assert_eq!(go(&["-s", "/a/one", "/dir"], &fs, &out), Ok(()));
    assert_eq!(
        fs.created(),
        [(String::from("/dir/one"), String::from("/a/one"))]
    );

    // With `-n` the same destination is the plain name it also is, and a
    // taken name still needs `-f`.
    let fs = TreeFs::with(&[("/dir", Made::LinkToDir)]);
    assert_eq!(
        go(&["-sn", "/a/one", "/dir"], &fs, &out),
        shown(&refused("/dir", "/a/one", true, Errno::AlreadyExists))
    );
    let fs = TreeFs::with(&[("/dir", Made::LinkToDir)]);
    assert_eq!(go(&["-snf", "/a/one", "/dir"], &fs, &out), Ok(()));
    assert_eq!(
        fs.created(),
        [(String::from("/dir"), String::from("/a/one"))]
    );
    assert_eq!(fs.replaced(), [String::from("/dir")]);
}

#[test]
fn no_target_directory_links_at_the_destination_itself() {
    let fs = TreeFs::with(&[("/dir", Made::Dir)]);
    let out = Recorder::default();
    // `-T` never fills a directory, so the existing directory blocks the name
    // rather than receiving a link.
    assert_eq!(
        go(&["-sT", "/a/one", "/dir"], &fs, &out),
        shown(&refused("/dir", "/a/one", true, Errno::AlreadyExists))
    );
    assert!(fs.created().is_empty());
}

#[test]
fn a_directory_is_never_replaced_even_under_force() {
    let fs = TreeFs::with(&[("/dir", Made::Dir)]);
    let out = Recorder::default();
    assert_eq!(
        go(&["-sfT", "/a/one", "/dir"], &fs, &out),
        shown(&refused("/dir", "/a/one", true, Errno::AlreadyExists))
    );
    assert!(fs.replaced().is_empty());
}

/// GNU: `target 'f' is not a directory`, and `failed to access 'x'` for a
/// `-t` directory that is not there.
#[test]
fn target_directory_must_already_be_a_directory() {
    let fs = TreeFs::with(&[("/file", Made::File)]);
    let out = Recorder::default();
    assert_eq!(
        go(&["-s", "-t", "/file", "a"], &fs, &out),
        shown(&LnError::NotADirectory(String::from("/file")))
    );
    let fs = TreeFs::default();
    assert_eq!(
        go(&["-s", "-t", "/absent", "a"], &fs, &out),
        shown(&LnError::Stat(String::from("/absent"), Errno::NotFound))
    );
    assert_eq!(
        LnError::Stat(String::from("/absent"), Errno::NotFound).to_string(),
        alloc::format!("failed to access '/absent': {}", Errno::NotFound)
    );
}

/// GNU: `target 'x': No such file or directory`, or `Not a directory`.
#[test]
fn several_targets_name_a_destination_that_cannot_hold_them() {
    let out = Recorder::default();
    let fs = TreeFs::default();
    let missing = LnError::Destination(String::from("/absent"), Errno::NotFound);
    assert_eq!(go(&["-s", "a", "b", "/absent"], &fs, &out), shown(&missing));
    assert_eq!(
        missing.to_string(),
        alloc::format!("target '/absent': {}", Errno::NotFound)
    );
    let fs = TreeFs::with(&[("/f", Made::File), ("/dl", Made::LinkToDir)]);
    assert_eq!(
        go(&["-s", "a", "b", "/f"], &fs, &out),
        shown(&LnError::Destination(
            String::from("/f"),
            Errno::NotADirectory
        ))
    );
    // `-n` makes a link to a directory the plain name it also is.
    assert_eq!(
        go(&["-sn", "a", "b", "/dl"], &fs, &out),
        shown(&LnError::Destination(
            String::from("/dl"),
            Errno::NotADirectory
        ))
    );
    assert!(fs.created().is_empty());
}

#[test]
fn a_taken_name_is_refused_by_default() {
    let fs = TreeFs::with(&[("/link", Made::File)]);
    let out = Recorder::default();
    assert_eq!(
        go(&["-s", "/a/target", "/link"], &fs, &out),
        shown(&refused("/link", "/a/target", true, Errno::AlreadyExists))
    );
    assert!(fs.created().is_empty());
    assert!(fs.replaced().is_empty());
}

#[test]
fn force_puts_the_new_link_in_the_taken_names_place() {
    let fs = TreeFs::with(&[("/link", Made::Link)]);
    let out = Recorder::default();
    assert_eq!(go(&["-sf", "/a/target", "/link"], &fs, &out), Ok(()));
    // The existing link is replaced itself, never written through.
    assert_eq!(fs.replaced(), [String::from("/link")]);
    assert_eq!(
        fs.created(),
        [(String::from("/link"), String::from("/a/target"))]
    );
}

/// GNU 9.4 ends a run whose `-i` question was declined with status 1.
#[test]
fn a_declined_question_changes_nothing_and_fails_the_run() {
    let fs = TreeFs::with(&[("/dir", Made::Dir), ("/dir/one", Made::File)]);
    let out = Recorder::answering(false);
    assert_eq!(
        go(&["-siv", "/a/one", "/b/two", "/dir"], &fs, &out),
        Err(String::new()),
        "failed, with nothing to say"
    );
    assert_eq!(*out.asked.borrow(), [String::from("replace '/dir/one'?")]);
    assert!(fs.replaced().is_empty());
    assert_eq!(
        fs.created(),
        [(String::from("/dir/two"), String::from("/b/two"))],
        "the next link is still made"
    );
    assert_eq!(out.text(), "'/dir/two' -> '/b/two'\n");
}

#[test]
fn interactive_replaces_on_consent() {
    let fs = TreeFs::with(&[("/link", Made::File)]);
    let out = Recorder::answering(true);
    assert_eq!(go(&["-siv", "/a/target", "/link"], &fs, &out), Ok(()));
    assert_eq!(fs.replaced(), [String::from("/link")]);
    assert_eq!(out.text(), "'/link' -> '/a/target'\n");
}

#[test]
fn verbose_reports_each_link() {
    let fs = TreeFs::with(&[("/dir", Made::Dir)]);
    let out = Recorder::default();
    assert_eq!(go(&["-sv", "/a/one", "/b/two", "/dir"], &fs, &out), Ok(()));
    assert_eq!(
        out.text(),
        "'/dir/one' -> '/a/one'\n'/dir/two' -> '/b/two'\n"
    );
}

#[test]
fn without_s_a_hard_link_is_made_and_no_symbolic_one() {
    // The default kind: a second directory entry for the target's own inode,
    // made through the hard-link call and never through the symbolic one.
    let fs = TreeFs::with(&[("/a/target", Made::File)]);
    let out = Recorder::default();
    assert_eq!(go(&["/a/target", "/link"], &fs, &out), Ok(()));
    assert_eq!(
        fs.hard_linked(),
        [(String::from("/link"), String::from("/a/target"), false)]
    );
    assert!(fs.created().is_empty(), "no symbolic link was made");
}

/// GNU marks a hard link `=>`, and names one it cannot make by both ends save
/// for a taken name or a directory target.
#[test]
fn a_hard_link_is_named_as_gnu_names_one() {
    let fs = TreeFs::with(&[("/a/target", Made::File)]);
    let out = Recorder::default();
    assert_eq!(go(&["-v", "/a/target", "/link"], &fs, &out), Ok(()));
    assert_eq!(out.text(), "'/link' => '/a/target'\n");
    let shapes = [
        (
            refused("/link", "/a/target", false, Errno::CrossVolume),
            alloc::format!(
                "failed to create hard link '/link' => '/a/target': {}",
                Errno::CrossVolume
            ),
        ),
        (
            refused("/link", "/a/target", false, Errno::AlreadyExists),
            alloc::format!(
                "failed to create hard link '/link': {}",
                Errno::AlreadyExists
            ),
        ),
        (
            refused("/link", "/a/target", false, Errno::IsADirectory),
            String::from("/a/target: hard link not allowed for directory"),
        ),
        (
            refused("/link", "/a/target", true, Errno::NotFound),
            alloc::format!(
                "failed to create symbolic link '/link': {}",
                Errno::NotFound
            ),
        ),
        (
            same_file("./a", "a"),
            String::from("'./a' and 'a' are the same file"),
        ),
    ];
    for (failure, words) in shapes {
        assert_eq!(failure.to_string(), words);
    }
}

/// A name's control bytes are escaped where GNU escapes them, never written
/// raw to the terminal.
#[test]
fn a_name_holding_a_control_byte_is_escaped_in_the_report() {
    let fs = TreeFs::with(&[("/a/t\u{1b}", Made::File)]);
    let out = Recorder::default();
    assert_eq!(go(&["-v", "/a/t\u{1b}", "/l\u{1b}[2J"], &fs, &out), Ok(()));
    assert_eq!(out.text(), "'/l'$'\\033''[2J' => '/a/t'$'\\033'\n");
}

#[test]
fn the_physical_posture_is_the_default_and_l_asks_to_follow() {
    // `-P` is what POSIX `link()` does and is the default; `-L` is
    // `linkat(AT_SYMLINK_FOLLOW)`, and the later switch wins.
    for (args, expected) in [
        (["-L", "/a/target", "/link"], true),
        (["-P", "/a/target", "/link"], false),
    ] {
        let fs = TreeFs::with(&[("/a/target", Made::Link)]);
        let out = Recorder::default();
        assert_eq!(go(&args, &fs, &out), Ok(()));
        assert_eq!(fs.hard_linked()[0].2, expected, "{args:?}");
    }
    let fs = TreeFs::with(&[("/a/target", Made::Link)]);
    let out = Recorder::default();
    assert_eq!(go(&["-LP", "/a/target", "/link"], &fs, &out), Ok(()));
    assert!(!fs.hard_linked()[0].2, "the later switch wins");
}

#[test]
fn d_and_f_accept_a_directory_operand_that_is_still_refused() {
    // GNU's `-d`/`-F` only stop `ln` refusing the command line; a directory is
    // never given a second name, and GNU says so before it looks at the link.
    for flag in ["-d", "-F"] {
        let fs = TreeFs::with(&[("/a/dir", Made::Dir), ("/link", Made::File)]);
        let out = Recorder::default();
        assert_eq!(
            go(&[flag, "/a/dir", "/link"], &fs, &out),
            shown(&refused("/link", "/a/dir", false, Errno::IsADirectory)),
            "{flag}"
        );
        assert!(fs.hard_linked().is_empty());
    }
}

/// GNU reaches a hard link's target first: `failed to access 'x'`, and a taken
/// name it is never given is kept, whatever `-f` said.
#[test]
fn a_missing_hard_link_target_is_reported_first_and_the_name_kept() {
    let missing = shown(&LnError::Stat(String::from("/a/missing"), Errno::NotFound));
    for args in [&["/a/missing", "/b"][..], &["-f", "/a/missing", "/b"][..]] {
        let fs = TreeFs::with(&[("/b", Made::File)]);
        let out = Recorder::default();
        assert_eq!(go(args, &fs, &out), missing, "{args:?}");
        assert!(fs.replaced().is_empty() && fs.holds("/b"));
    }
    // `-L` through a link that names nothing is the same failure.
    let fs = TreeFs::with(&[("/dang", Made::Dangling)]);
    let out = Recorder::default();
    assert_eq!(
        go(&["-L", "/dang", "/x"], &fs, &out),
        shown(&LnError::Stat(String::from("/dang"), Errno::NotFound))
    );
}

/// GNU refuses to force a file onto its own entry, however it is spelled.
#[test]
fn forcing_a_file_onto_its_own_entry_is_refused_and_the_file_kept() {
    let fs = TreeFs::with(&[("a", Made::File)]).link_to("alink", "a");
    let out = Recorder::default();
    for args in [
        ["-f", "a", "a"],
        ["-sf", "a", "a"],
        ["-f", "./a", "a"],
        ["-f", "a", "d/../a"],
        ["-fL", "alink", "a"],
        ["-fP", "alink", "alink"],
    ] {
        assert_eq!(
            go(&args, &fs, &out),
            shown(&same_file(args[1], args[2])),
            "{args:?}"
        );
    }
    assert!(fs.replaced().is_empty() && fs.holds("a"));
    assert!(fs.hard_linked().is_empty() && fs.created().is_empty());
}

/// GNU takes a name that already is the target's node as linked: `-v` still
/// reports it, and nothing is touched.
#[test]
fn a_name_already_linked_to_the_target_is_left_as_it_is() {
    let fs = TreeFs::with(&[("a", Made::File)]).second_name("h", "a");
    let out = Recorder::default();
    assert_eq!(go(&["-fv", "a", "h"], &fs, &out), Ok(()));
    assert_eq!(out.text(), "'h' => 'a'\n");
    assert!(fs.replaced().is_empty() && fs.hard_linked().is_empty());
}

/// GNU 9.4 asks first, and a consented hard link of a file onto itself is
/// already done.
#[test]
fn consenting_to_link_a_file_onto_itself_leaves_it() {
    let fs = TreeFs::with(&[("b", Made::File)]);
    let out = Recorder::answering(true);
    assert_eq!(go(&["-iv", "b", "b"], &fs, &out), Ok(()));
    assert_eq!(out.text(), "'b' => 'b'\n");
    assert!(fs.replaced().is_empty() && fs.holds("b"));
}

/// A symbolic link onto another name of the file it names is made: the file
/// keeps its other name.
#[test]
fn a_symbolic_link_onto_another_name_of_its_target_is_made() {
    let fs = TreeFs::with(&[("a", Made::File)]).second_name("h", "a");
    let out = Recorder::default();
    assert_eq!(go(&["-sf", "a", "h"], &fs, &out), Ok(()));
    assert_eq!(fs.replaced(), [String::from("h")]);
}

/// The link reads its target from its own directory, so a target naming the
/// very file the link replaces is refused, even after an `-i` consent — where
/// GNU judges from the working directory and destroys the file.
#[test]
fn a_symbolic_link_that_would_name_the_file_it_replaces_is_refused() {
    let fs = TreeFs::with(&[("/d/a", Made::File), ("a", Made::File)]);
    let out = Recorder::answering(true);
    let refused_here = shown(&same_file("a", "/d/a"));
    assert_eq!(go(&["-sf", "a", "/d/a"], &fs, &out), refused_here);
    assert_eq!(go(&["-si", "a", "/d/a"], &fs, &out), refused_here);
    assert!(fs.replaced().is_empty() && fs.created().is_empty());
}

#[test]
fn a_format_with_one_name_per_node_reports_the_permanent_limit() {
    let fs = TreeFs::with(&[("/a/target", Made::File)]).refusing(Errno::NotSupported);
    let out = Recorder::default();
    assert_eq!(
        go(&["/a/target", "/link"], &fs, &out),
        shown(&refused("/link", "/a/target", false, Errno::NotSupported))
    );
    assert!(fs.hard_linked().is_empty());
}

#[test]
fn a_cross_volume_hard_link_reports_that_refusal() {
    // The mover's own signal: a second directory entry cannot address an
    // inode in another backing, so `ln` reports it rather than copying.
    let fs = TreeFs::with(&[("/a/target", Made::File)]).refusing(Errno::CrossVolume);
    let out = Recorder::default();
    assert_eq!(
        go(&["/a/target", "/other/link"], &fs, &out),
        shown(&refused(
            "/other/link",
            "/a/target",
            false,
            Errno::CrossVolume
        ))
    );
}

#[test]
fn a_format_without_links_reports_the_permanent_limit() {
    let fs = TreeFs::default().refusing(Errno::NotSupported);
    let out = Recorder::default();
    assert_eq!(
        go(&["-s", "/a/target", "/link"], &fs, &out),
        shown(&refused("/link", "/a/target", true, Errno::NotSupported))
    );
}

/// GNU 9.4 reports a link it cannot make and still makes the others.
#[test]
fn a_failed_link_is_reported_and_the_next_still_made() {
    let fs = TreeFs::with(&[("/dir", Made::Dir), ("/dir/two", Made::File)]);
    let out = Recorder::default();
    assert_eq!(
        go(&["-s", "/a/one", "/b/two", "/c/three", "/dir"], &fs, &out),
        shown(&refused("/dir/two", "/b/two", true, Errno::AlreadyExists))
    );
    assert_eq!(
        fs.created(),
        [
            (String::from("/dir/one"), String::from("/a/one")),
            (String::from("/dir/three"), String::from("/c/three"))
        ]
    );
}

#[test]
fn help_falls_back_to_the_usage_banner_without_a_tree() {
    let fs = TreeFs::default();
    let out = Recorder::default();
    assert_eq!(go(&["-h"], &fs, &out), Ok(()));
    assert_eq!(out.text(), USAGE);
}

#[test]
fn relative_spelling_walks_up_then_down_between_two_canonical_paths() {
    // Both inputs are canonical, so the arithmetic is exact: shared prefix
    // dropped, one `..` per remaining base component, then the rest of the
    // target.
    for (base, target, want) in [
        ("/a/b", "/a/b/c", "c"),
        ("/a/b/c", "/a/b", ".."),
        ("/a/b/c", "/a/x/y", "../../x/y"),
        ("/a/b", "/a/b", "."),
        ("/", "/a", "a"),
        ("/a", "/", ".."),
        ("/a/b", "/c", "../../c"),
    ] {
        assert_eq!(relative_spelling(base, target), want, "{base} -> {target}");
    }
}

#[test]
fn relative_stores_the_target_relative_to_the_links_own_directory() {
    // The kernel canonicalises both halves; `-r` only spells the difference.
    // Here `/a/b/link` sits in `/a/b`, and the target canonicalises out of a
    // link, so a lexical answer from the operands as typed would have been
    // `../real/file` — a different node.
    let fs =
        TreeFs::default().canonicalising(&[("/a/alias/file", "/x/real/file"), ("/a/b", "/a/b")]);
    let out = Recorder::default();
    assert_eq!(
        go(&["-sr", "/a/alias/file", "/a/b/link"], &fs, &out),
        Ok(())
    );
    assert_eq!(
        fs.created(),
        [(String::from("/a/b/link"), String::from("../../x/real/file"))]
    );
}

#[test]
fn relative_reports_the_stored_target_not_the_operand() {
    // `-v` must show what the link actually holds, or the report would
    // describe a link that was never made.
    let fs = TreeFs::default();
    let out = Recorder::default();
    assert_eq!(go(&["-srv", "/a/b/target", "/a/b/link"], &fs, &out), Ok(()));
    assert_eq!(out.text(), "'/a/b/link' -> 'target'\n");
}

#[test]
fn relative_without_symbolic_is_refused() {
    // A hard link stores no target, so there is nothing to make relative:
    // refused rather than silently ignored.
    assert_eq!(
        parse(&["-r", "a", "b"]),
        Err(LnError::RelativeNeedsSymbolic)
    );
    assert_eq!(
        parse(&["--relative", "a", "b"]),
        Err(LnError::RelativeNeedsSymbolic)
    );
}

#[test]
fn a_refused_canonicalisation_names_the_path() {
    let fs = TreeFs {
        canonicalize_errno: Some(Errno::PermissionDenied),
        ..TreeFs::default()
    };
    let out = Recorder::default();
    assert_eq!(
        go(&["-sr", "/a/target", "/b/link"], &fs, &out),
        shown(&LnError::Canonicalize(
            String::from("/a/target"),
            Errno::PermissionDenied
        ))
    );
    assert!(fs.created().is_empty(), "nothing was created");
}

#[test]
fn an_inspection_failure_names_the_path() {
    struct Refusing;
    impl FileSystem for Refusing {
        fn node(&self, _path: &str, _link: FinalLink) -> Result<Option<Node>, Errno> {
            Err(Errno::PermissionDenied)
        }
        fn symlink(&self, _target: &str, _link: &str, _replace: bool) -> Result<(), Errno> {
            Err(Errno::PermissionDenied)
        }
        fn link(&self, _: &str, _: &str, _deref: bool, _replace: bool) -> Result<(), Errno> {
            Err(Errno::PermissionDenied)
        }
        fn canonicalize(&self, _path: &str, _mode: RealpathMode) -> Result<String, Errno> {
            Err(Errno::PermissionDenied)
        }
    }
    let out = Recorder::default();
    let command = parse(&["-s", "/a/target", "/b/link"]).expect("parse");
    // The destination cannot be inspected: nothing can be decided, so the run
    // ends.
    assert_eq!(
        run(
            command,
            None,
            &Refusing,
            &out,
            &NoHelp,
            &out,
            &Recorder::default()
        ),
        Err(LnError::Stat(
            String::from("/b/link"),
            Errno::PermissionDenied
        ))
    );
}
