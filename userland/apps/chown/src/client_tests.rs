use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use tairix_abi::fs::FS_OWNER_UNCHANGED;
use tairix_abi::Errno;
use tairix_fscopy::fixture::{Tree, USER};
use tairix_help::{HelpSource, SourceError};

use super::{run, USAGE};
use crate::command::parse;
use crate::error::ChownError;
use crate::io::{Account, Accounts, Output};

/// The directories names are read against: `me` is the caller, the tree's
/// own user, and `alice` an account whose login group is `staff`.
struct Names;

impl Accounts for Names {
    fn user(&self, name: &str) -> Result<Option<Account>, Errno> {
        Ok(match name {
            "me" => Some(Account {
                uid: USER.0,
                login_group: USER.1,
            }),
            "alice" => Some(Account {
                uid: 1001,
                login_group: 50,
            }),
            _ => None,
        })
    }

    fn group(&self, name: &str) -> Result<Option<u32>, Errno> {
        Ok(match name {
            "me" => Some(USER.1),
            "staff" => Some(50),
            _ => None,
        })
    }

    fn user_name(&self, uid: u32) -> Result<Option<String>, Errno> {
        Ok(match uid {
            1000 => Some(String::from("me")),
            1001 => Some(String::from("alice")),
            _ => None,
        })
    }

    fn group_name(&self, gid: u32) -> Result<Option<String>, Errno> {
        Ok(match gid {
            1000 => Some(String::from("me")),
            50 => Some(String::from("staff")),
            _ => None,
        })
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

/// A Help tree holding one canonical `chown.md` document.
struct OneDoc;

const DOC: &str = "## NAME\n\nchown — change file owner and group\n\n\
                   ## SYNOPSIS\n\n`chown [OPTION]... [OWNER][:[GROUP]] file...`\n\n\
                   ## DESCRIPTION\n\nChanges owners.\n";

impl HelpSource for OneDoc {
    fn locale_dirs(&self) -> Result<Vec<String>, SourceError> {
        Ok(alloc::vec![String::from("en-US")])
    }

    fn read(&self, locale_dir: &str, file_name: &str) -> Result<Option<Vec<u8>>, SourceError> {
        Ok((locale_dir == "en-US" && file_name == "chown.md").then(|| DOC.as_bytes().to_vec()))
    }
}

/// A stream that records every byte written to it.
#[derive(Default)]
struct Recorder(RefCell<Vec<u8>>);

impl Recorder {
    fn text(&self) -> String {
        String::from_utf8(self.0.borrow().clone()).expect("utf-8 output")
    }
}

impl Output for Recorder {
    fn write_all(&self, bytes: &[u8]) -> Result<(), Errno> {
        self.0.borrow_mut().extend_from_slice(bytes);
        Ok(())
    }
}

/// What one run answered and wrote on each stream.
struct Ran {
    status: Result<bool, ChownError>,
    out: String,
    err: String,
}

fn chown(args: &[&str], tree: &Tree) -> Ran {
    let out = Recorder::default();
    let err = Recorder::default();
    let command = parse(args).expect("a command line chown reads");
    let status = run(command, None, tree, &Names, &NoHelp, &out, &err);
    Ran {
        status,
        out: out.text(),
        err: err.text(),
    }
}

/// The calls one unattacked run of `args` makes against the tree `build`
/// makes, so a sweep can attack before each of them in turn.
fn calls_of(args: &[&str], build: fn() -> Tree) -> usize {
    let tree = build();
    chown(args, &tree);
    tree.calls().len()
}

/// A tree whose links lead out of it, to nodes another user may own.
fn links_tree() -> Tree {
    Tree::new()
        .privileged()
        .dir("/d")
        .link("/d/dl", "/vdir")
        .link("/d/l", "/victim")
        .file("/victim", b"")
        .dir("/vdir")
        .file("/vdir/x", b"")
}

/// A tree to walk, its victims outside it, and the links an attacker swaps
/// into it.
fn bait_tree() -> Tree {
    Tree::new()
        .privileged()
        .dir("/d")
        .file("/d/f", b"")
        .dir("/d/sub")
        .file("/d/sub/g", b"")
        .file("/victim", b"")
        .dir("/vdir")
        .file("/vdir/x", b"")
        .link("/bait-f", "/victim")
        .link("/bait-sub", "/vdir")
}

/// `/f` to change and `/other`, owned elsewhere, to swap in for it.
fn swap_tree() -> Tree {
    Tree::new()
        .privileged()
        .file("/f", b"")
        .file("/other", b"")
        .owned("/other", 2000, 2000)
}

/// `/f` to change and a link to a victim to swap in for it.
fn link_swap_tree() -> Tree {
    Tree::new()
        .privileged()
        .file("/f", b"")
        .file("/victim", b"")
        .link("/bait", "/victim")
}

#[test]
fn help_prints_the_usage_banner_without_a_document() {
    let ran = chown(&["--help"], &Tree::new());
    assert_eq!(ran.status, Ok(true));
    assert_eq!(ran.out, USAGE);
}

#[test]
fn help_renders_the_short_help_from_the_document() {
    let out = Recorder::default();
    let command = parse(&["-?"]).expect("help");
    let status = run(
        command,
        None,
        &Tree::new(),
        &Names,
        &OneDoc,
        &out,
        &Recorder::default(),
    );
    assert_eq!(status, Ok(true));
    let written = out.text();
    assert!(
        written.contains("chown — change file owner and group"),
        "{written}"
    );
    assert!(
        written.contains("chown [OPTION]... [OWNER][:[GROUP]] file..."),
        "{written}"
    );
}

#[test]
fn version_names_the_tool_and_its_version() {
    let ran = chown(&["--version"], &Tree::new());
    assert_eq!(ran.status, Ok(true));
    assert_eq!(
        ran.out,
        format!("chown (TAIRiX) {}\n", env!("CARGO_PKG_VERSION"))
    );
}

/// GNU's manual: an owner alone keeps the group, `:GROUP` keeps the user.
#[test]
fn an_owner_alone_keeps_the_group_and_a_group_alone_keeps_the_user() {
    let tree = Tree::new()
        .privileged()
        .file("/a", b"")
        .file("/b", b"")
        .owned("/a", 1, 2)
        .owned("/b", 1, 2);
    assert_eq!(chown(&["0", "/a"], &tree).status, Ok(true));
    assert_eq!(tree.owner("/a"), Some((0, 2)));
    assert_eq!(chown(&[":50", "/b"], &tree).status, Ok(true));
    assert_eq!(tree.owner("/b"), Some((1, 50)));
}

#[test]
fn names_are_read_before_anything_changes() {
    let tree = Tree::new().privileged().file("/f", b"");
    assert_eq!(chown(&["alice:staff", "/f"], &tree).status, Ok(true));
    assert_eq!(tree.owner("/f"), Some((1001, 50)));
    let ran = chown(&["nosuch", "/f"], &tree);
    assert_eq!(ran.status, Err(ChownError::BadUser(String::from("nosuch"))));
    assert_eq!(tree.owner("/f"), Some((1001, 50)));
}

/// GNU's manual: `OWNER:` gives a named owner its login group, whichever
/// account it names.
#[test]
fn owner_colon_gives_any_named_owner_its_login_group() {
    let tree = Tree::new().privileged().file("/f", b"").owned("/f", 7, 7);
    assert_eq!(chown(&["me:", "/f"], &tree).status, Ok(true));
    assert_eq!(tree.owner("/f"), Some(USER));
    assert_eq!(chown(&["alice:", "/f"], &tree).status, Ok(true));
    assert_eq!(tree.owner("/f"), Some((1001, 50)));
}

#[test]
fn the_old_dot_separator_warns_and_still_changes() {
    let tree = Tree::new().privileged().file("/f", b"");
    let ran = chown(&["alice.staff", "/f"], &tree);
    assert_eq!(ran.status, Ok(true));
    assert_eq!(ran.err, "chown: warning: '.' should be ':'\n");
    assert_eq!(tree.owner("/f"), Some((1001, 50)));
}

/// GNU's manual: without `-R` a link operand's referent changes, and with
/// `-h` the link itself.
#[test]
fn a_link_operand_changes_what_it_names_and_under_h_itself() {
    let tree = Tree::new().privileged().file("/t", b"").link("/l", "/t");
    assert_eq!(chown(&["0", "/l"], &tree).status, Ok(true));
    assert_eq!(tree.owner("/t"), Some((0, USER.1)));
    assert_eq!(tree.owner("/l"), Some(USER));
    assert_eq!(chown(&["-h", "1", "/l"], &tree).status, Ok(true));
    assert_eq!(tree.owner("/l"), Some((1, USER.1)));
    assert_eq!(tree.owner("/t"), Some((0, USER.1)));
}

#[test]
fn a_dangling_link_operand_is_named_dangling_unless_h_changes_it() {
    let tree = Tree::new().privileged().link("/dl", "/nowhere");
    let ran = chown(&["-v", "0", "/dl"], &tree);
    assert_eq!(ran.status, Ok(false));
    assert_eq!(
        ran.err,
        format!("chown: cannot dereference '/dl': {}\n", Errno::NotFound)
    );
    assert_eq!(ran.out, "failed to change ownership of '/dl' to 0\n");
    assert_eq!(chown(&["-h", "0", "/dl"], &tree).status, Ok(true));
    assert_eq!(tree.owner("/dl"), Some((0, USER.1)));
}

#[test]
fn without_recursive_only_the_named_directory_changes() {
    let tree = Tree::new().privileged().dir("/d").file("/d/f", b"");
    assert_eq!(chown(&["9:9", "/d"], &tree).status, Ok(true));
    assert_eq!(tree.owner("/d"), Some((9, 9)));
    assert_eq!(tree.owner("/d/f"), Some(USER));
}

#[test]
fn recursive_changes_each_directory_after_its_contents() {
    let tree = Tree::new()
        .privileged()
        .dir("/d")
        .file("/d/f", b"")
        .dir("/d/sub")
        .file("/d/sub/g", b"");
    let ran = chown(&["-Rv", "0", "/d"], &tree);
    assert_eq!(ran.status, Ok(true));
    assert_eq!(
        ran.out,
        "changed ownership of '/d/f' from me to 0\n\
         changed ownership of '/d/sub/g' from me to 0\n\
         changed ownership of '/d/sub' from me to 0\n\
         changed ownership of '/d' from me to 0\n"
    );
}

/// A directory given away before its contents would shut a caller whose
/// authority grants no reach into it out of the rest of the walk.
#[test]
fn a_directory_given_away_is_walked_before_it_changes() {
    let tree = Tree::new()
        .privileged()
        .checking_permissions()
        .dir("/d")
        .dir("/d/sub")
        .file("/d/sub/f", b"")
        .with_mode("/d", 0o700)
        .with_mode("/d/sub", 0o700);
    assert_eq!(chown(&["-R", "2000", "/d"], &tree).status, Ok(true));
    for path in ["/d", "/d/sub", "/d/sub/f"] {
        assert_eq!(tree.owner(path), Some((2000, USER.1)), "{path}");
    }
}

/// D1422: GNU's `chown -R` changes a link it meets itself, and nothing the
/// link names is reached.
#[test]
fn recursive_changes_a_link_it_meets_itself_never_what_it_names() {
    let tree = links_tree();
    assert_eq!(chown(&["-R", "0", "/d"], &tree).status, Ok(true));
    for link in ["/d/dl", "/d/l"] {
        assert_eq!(tree.owner(link), Some((0, USER.1)), "{link}");
    }
    for theirs in ["/victim", "/vdir", "/vdir/x"] {
        assert_eq!(tree.owner(theirs), Some(USER), "{theirs}");
    }
}

#[test]
fn dereference_leaves_a_link_the_walk_meets_alone_with_what_it_names() {
    let tree = links_tree();
    let ran = chown(&["-Rv", "--dereference", "0", "/d"], &tree);
    assert_eq!(ran.status, Ok(true));
    assert_eq!(
        ran.out,
        "neither symbolic link '/d/dl' nor referent has been changed\n\
         neither symbolic link '/d/l' nor referent has been changed\n\
         changed ownership of '/d' from me to 0\n"
    );
    for untouched in ["/d/dl", "/d/l", "/victim", "/vdir", "/vdir/x"] {
        assert_eq!(tree.owner(untouched), Some(USER), "{untouched}");
    }
}

/// GNU's manual: `-P`, the default, traverses no link, and `-H` a link
/// operand that names a directory.
#[test]
fn recursive_changes_a_link_operand_itself_and_h_walks_what_it_names() {
    let tree = Tree::new()
        .privileged()
        .dir("/vdir")
        .file("/vdir/x", b"")
        .link("/l", "/vdir");
    assert_eq!(chown(&["-R", "0", "/l"], &tree).status, Ok(true));
    assert_eq!(tree.owner("/l"), Some((0, USER.1)));
    assert_eq!(tree.owner("/vdir"), Some(USER));
    assert_eq!(tree.owner("/vdir/x"), Some(USER));
    assert_eq!(chown(&["-RH", "1", "/l"], &tree).status, Ok(true));
    assert_eq!(tree.owner("/vdir"), Some((1, USER.1)));
    assert_eq!(tree.owner("/vdir/x"), Some((1, USER.1)));
    assert_eq!(tree.owner("/l"), Some((0, USER.1)));
}

#[test]
fn recursive_dereference_changes_what_a_link_operand_names_without_walking_it() {
    let tree = Tree::new()
        .privileged()
        .dir("/vdir")
        .file("/vdir/x", b"")
        .link("/l", "/vdir");
    assert_eq!(
        chown(&["-R", "--dereference", "0", "/l"], &tree).status,
        Ok(true)
    );
    assert_eq!(tree.owner("/vdir"), Some((0, USER.1)));
    assert_eq!(tree.owner("/vdir/x"), Some(USER));
    assert_eq!(tree.owner("/l"), Some(USER));
}

#[test]
fn h_changes_a_link_operand_to_a_file_itself_unless_dereference_is_asked() {
    let tree = Tree::new().privileged().file("/t", b"").link("/fl", "/t");
    assert_eq!(chown(&["-RH", "0", "/fl"], &tree).status, Ok(true));
    assert_eq!(tree.owner("/fl"), Some((0, USER.1)));
    assert_eq!(tree.owner("/t"), Some(USER));
    assert_eq!(
        chown(&["-RH", "--dereference", "1", "/fl"], &tree).status,
        Ok(true)
    );
    assert_eq!(tree.owner("/t"), Some((1, USER.1)));
}

#[test]
fn from_changes_only_a_node_owned_as_it_names() {
    let tree = Tree::new()
        .privileged()
        .file("/a", b"")
        .file("/b", b"")
        .owned("/b", 2000, 2000);
    let ran = chown(&["-v", "--from=1000", "0", "/a", "/b"], &tree);
    assert_eq!(ran.status, Ok(true));
    assert_eq!(tree.owner("/a"), Some((0, USER.1)));
    assert_eq!(tree.owner("/b"), Some((2000, 2000)));
    assert_eq!(
        ran.out,
        "changed ownership of '/a' from me to 0\nownership of '/b' retained as 2000\n"
    );
    assert_eq!(
        chown(&["--from=:2000", "5", "/a", "/b"], &tree).status,
        Ok(true)
    );
    assert_eq!(tree.owner("/a"), Some((0, USER.1)));
    assert_eq!(tree.owner("/b"), Some((5, 2000)));
}

/// The owner `--from` reads and the change it allows are one node's, so a
/// node swapped in under the name at any point is never changed.
#[test]
fn from_is_judged_on_the_node_it_changes() {
    let args = ["--from=1000", "0", "/f"];
    for n in 0..calls_of(&args, swap_tree) {
        let tree = swap_tree();
        tree.before_call(
            n,
            Box::new(|tree: &Tree| {
                tree.rename("/f", "/gone");
                tree.rename("/other", "/f");
            }),
        );
        chown(&args, &tree);
        let other = if tree.exists("/other") {
            "/other"
        } else {
            "/f"
        };
        assert_eq!(
            tree.owner(other),
            Some((2000, 2000)),
            "swapped before call {n}"
        );
    }
}

/// D1422: an entry swapped for a link before any seam call of a walk never
/// carries the change out of the tree.
#[test]
fn a_name_swapped_for_a_link_at_any_point_never_carries_the_change_out() {
    let runs: [&[&str]; 3] = [
        &["-R", "0", "/d"],
        &["-RH", "0", "/d"],
        &["-R", "--dereference", "0", "/d"],
    ];
    for args in runs {
        for n in 0..calls_of(args, bait_tree) {
            let tree = bait_tree();
            tree.before_call(
                n,
                Box::new(|tree: &Tree| {
                    tree.rename("/d/f", "/gone-f");
                    tree.rename("/bait-f", "/d/f");
                    tree.rename("/d/sub", "/gone-sub");
                    tree.rename("/bait-sub", "/d/sub");
                }),
            );
            chown(args, &tree);
            for theirs in ["/victim", "/vdir", "/vdir/x"] {
                assert_eq!(
                    tree.owner(theirs),
                    Some(USER),
                    "{args:?}: {theirs} after a swap before call {n}"
                );
            }
        }
    }
}

/// `-h` names the link itself, so a link swapped in for the operand is
/// changed rather than followed.
#[test]
fn h_never_reaches_what_a_swapped_in_link_names() {
    let args = ["-h", "0", "/f"];
    for n in 0..calls_of(&args, link_swap_tree) {
        let tree = link_swap_tree();
        tree.before_call(
            n,
            Box::new(|tree: &Tree| {
                tree.rename("/f", "/gone");
                tree.rename("/bait", "/f");
            }),
        );
        chown(&args, &tree);
        assert_eq!(tree.owner("/victim"), Some(USER), "swapped before call {n}");
    }
}

#[test]
fn reference_gives_the_owner_and_group_of_what_it_names() {
    let tree = Tree::new()
        .privileged()
        .file("/r", b"")
        .owned("/r", 7, 8)
        .link("/rl", "/r")
        .file("/f", b"");
    assert_eq!(chown(&["--reference=/rl", "/f"], &tree).status, Ok(true));
    assert_eq!(tree.owner("/f"), Some((7, 8)));
    assert_eq!(
        chown(&["--reference", "/missing", "/f"], &tree).status,
        Err(ChownError::Reference {
            path: String::from("/missing"),
            errno: Errno::NotFound,
        })
    );
    assert_eq!(tree.owner("/f"), Some((7, 8)));
}

/// GNU 9.4: `chown 5:5 missing b` names the missing one and still changes
/// `b`; under `-v` it names the change it could not make.
#[test]
fn a_missing_operand_is_reported_and_the_next_still_changed() {
    let tree = Tree::new().privileged().file("/b", b"");
    let ran = chown(&["-v", "5:5", "/a", "/b"], &tree);
    assert_eq!(ran.status, Ok(false));
    assert_eq!(ran.err, "chown: cannot access '/a': not found\n");
    assert_eq!(
        ran.out,
        "failed to change ownership of '/a' to 5:5\n\
         changed ownership of '/b' from me:me to 5:5\n"
    );
    assert_eq!(tree.owner("/b"), Some((5, 5)));
    let ran = chown(&["-v", ":5", "/a"], &tree);
    assert_eq!(ran.out, "failed to change group of '/a' to 5\n");
}

#[test]
fn a_change_the_caller_may_not_make_is_reported_and_the_rest_still_tried() {
    let tree = Tree::new()
        .file("/a", b"")
        .file("/b", b"")
        .owned("/b", USER.0, 7);
    let ran = chown(&["-v", "0", "/a", "/b"], &tree);
    assert_eq!(ran.status, Ok(false));
    assert_eq!(
        ran.err,
        "chown: changing ownership of '/a': permission denied\n\
         chown: changing ownership of '/b': permission denied\n"
    );
    assert_eq!(
        ran.out,
        "failed to change ownership of '/a' from me to 0\n\
         failed to change ownership of '/b' from me to 0\n"
    );
    let ran = chown(&["-c", ":1000", "/a", "/b"], &tree);
    assert_eq!(ran.status, Ok(true));
    assert_eq!(ran.out, "changed group of '/b' from 7 to me\n");
    assert_eq!(tree.owner("/b"), Some(USER));
}

#[test]
fn an_entry_that_fails_is_reported_and_the_rest_of_its_directory_still_changed() {
    let tree = Tree::new()
        .privileged()
        .dir("/d")
        .dir("/d/a")
        .file("/d/a/f", b"")
        .file("/d/b", b"")
        .dir("/d/c")
        .file("/d/c/g", b"")
        .file("/d/e", b"")
        .refuse("set_owner /d/a", Errno::PermissionDenied)
        .refuse("entries /d/c", Errno::PermissionDenied);
    let ran = chown(&["-R", "0", "/d"], &tree);
    assert_eq!(ran.status, Ok(false));
    assert_eq!(
        ran.err,
        "chown: changing ownership of '/d/a': permission denied\n\
         chown: cannot read directory '/d/c': permission denied\n"
    );
    for changed in ["/d", "/d/a/f", "/d/b", "/d/c", "/d/e"] {
        assert_eq!(tree.owner(changed), Some((0, USER.1)), "{changed}");
    }
    for kept in ["/d/a", "/d/c/g"] {
        assert_eq!(tree.owner(kept), Some(USER), "{kept}");
    }
}

#[test]
fn an_operand_that_cannot_be_listed_is_still_changed_and_reported() {
    let tree = Tree::new()
        .privileged()
        .dir("/d")
        .file("/d/f", b"")
        .refuse("entries /d", Errno::PermissionDenied);
    let ran = chown(&["-R", "0", "/d"], &tree);
    assert_eq!(ran.status, Ok(false));
    assert_eq!(
        ran.err,
        "chown: cannot read directory '/d': permission denied\n"
    );
    assert_eq!(tree.owner("/d"), Some((0, USER.1)));
    assert_eq!(tree.owner("/d/f"), Some(USER));
}

#[test]
fn an_operand_that_will_not_open_to_list_is_still_changed_and_reported() {
    let tree = Tree::new()
        .privileged()
        .dir("/d")
        .file("/d/f", b"")
        .refuse("open_dir /d", Errno::PermissionDenied);
    let ran = chown(&["-R", "0", "/d"], &tree);
    assert_eq!(ran.status, Ok(false));
    assert_eq!(
        ran.err,
        format!(
            "chown: cannot read directory '/d': {}\n",
            Errno::PermissionDenied
        )
    );
    assert_eq!(tree.owner("/d"), Some((0, USER.1)));
    assert_eq!(tree.owner("/d/f"), Some(USER));
}

#[test]
fn preserve_root_refuses_the_view_root_however_reached() {
    let volume = "id::b7f2e4e6-8d7a-4ef8-a13e-d3b84d4e8001";
    let tree = Tree::new()
        .privileged()
        .file("/f", b"")
        .named_root(volume, "/");
    let by_volume = format!("{volume}/");
    for spelled in ["/", "//", by_volume.as_str()] {
        let ran = chown(&["-R", "--preserve-root", "0", spelled], &tree);
        assert_eq!(ran.status, Ok(false), "{spelled}");
        let same = if spelled == "/" { "" } else { " (same as '/')" };
        assert_eq!(
            ran.err,
            format!(
                "chown: it is dangerous to operate recursively on '{spelled}'{same}\n\
                 chown: use --no-preserve-root to override this failsafe\n"
            )
        );
        assert_eq!(tree.owner("/f"), Some(USER), "{spelled}");
    }
    assert_eq!(chown(&["-R", "0", "/"], &tree).status, Ok(true));
    assert_eq!(
        tree.owner("/f"),
        Some((0, USER.1)),
        "no failsafe unless asked"
    );
}

#[test]
fn an_id_the_system_reads_as_unchanged_is_never_asked_for() {
    let tree = Tree::new()
        .privileged()
        .file("/r", b"")
        .owned("/r", FS_OWNER_UNCHANGED, 5)
        .file("/f", b"");
    let ran = chown(&["--reference=/r", "/f"], &tree);
    assert_eq!(
        ran.status.map_err(|refusal| format!("{refusal}")),
        Err(format!(
            "failed to get attributes of '/r': {}",
            Errno::OutOfRange
        ))
    );
    assert_eq!(tree.owner("/f"), Some(USER));
}

#[test]
fn verbose_reports_changed_and_retained_in_gnu_wording() {
    let tree = Tree::new()
        .privileged()
        .file("/f", b"")
        .owned("/f", 1000, 100);
    assert_eq!(
        chown(&["-v", "0:0", "/f"], &tree).out,
        "changed ownership of '/f' from me:100 to 0:0\n"
    );
    assert_eq!(
        chown(&["-v", "0:0", "/f"], &tree).out,
        "ownership of '/f' retained as 0:0\n"
    );
    assert_eq!(
        chown(&["-v", ":", "/f"], &tree).out,
        "ownership of '/f' retained\n"
    );
}

#[test]
fn changes_reports_only_an_actual_change_in_the_operand_s_shape() {
    let tree = Tree::new()
        .privileged()
        .file("/f", b"")
        .owned("/f", 1000, 100);
    assert_eq!(chown(&["-c", "1000", "/f"], &tree).out, "");
    assert_eq!(
        chown(&["-c", "0", "/f"], &tree).out,
        "changed ownership of '/f' from me to 0\n"
    );
    assert_eq!(
        chown(&["-v", ":100", "/f"], &tree).out,
        "group of '/f' retained as 100\n"
    );
    assert_eq!(
        chown(&["-c", ":5", "/f"], &tree).out,
        "changed group of '/f' from 100 to 5\n"
    );
}

#[test]
fn quiet_suppresses_a_failure_but_still_fails_the_run() {
    let tree = Tree::new().privileged().file("/b", b"");
    let ran = chown(&["-f", "0:0", "/a", "/b"], &tree);
    assert_eq!(ran.status, Ok(false));
    assert_eq!(ran.err, "");
    assert_eq!(tree.owner("/b"), Some((0, 0)));
}

/// A name's control bytes are escaped where GNU escapes them, never written
/// raw to the terminal.
#[test]
fn a_report_names_its_file_escaped() {
    let tree = Tree::new()
        .privileged()
        .file("/f\u{1b}", b"")
        .owned("/f\u{1b}", 1000, 100);
    assert_eq!(
        chown(&["-v", "0:0", "/f\u{1b}"], &tree).out,
        "changed ownership of '/f'$'\\033' from me:100 to 0:0\n"
    );
}
