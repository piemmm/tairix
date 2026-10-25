use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::{Errno, FileKind};

use super::{
    open_operand, walk, Cursor, Dir, Entry, Kind, Node, Operand, Reached, Roots, Step, Unopened,
    Visitor,
};
use crate::fixture::{Tree, TreeDir};

/// Every call a walk makes of its visitor, in order, descending into every
/// directory listed — or, with `everything`, every entry.
#[derive(Default)]
struct Seen {
    events: Vec<String>,
    everything: bool,
}

impl<D: Dir> Visitor<D> for Seen {
    type Error = Errno;

    fn enter(&mut self, _: &D, entry: &Entry, path: &str) -> Result<Step<D>, Errno> {
        self.events.push(format!("enter {path} {:?}", entry.kind));
        Ok(if self.everything || entry.kind == Kind::Directory {
            Step::Descend
        } else {
            Step::Skip
        })
    }

    fn leave(&mut self, _: &D, _: &Entry, path: &str, held: &D) -> Result<(), Errno> {
        let kind = held.stat()?.kind;
        self.events.push(format!("leave {path} {kind:?}"));
        Ok(())
    }

    fn refused(&mut self, _: &D, _: &Entry, path: &str, errno: Errno) -> Result<(), Errno> {
        self.events.push(format!("refused {path} {errno:?}"));
        Ok(())
    }

    fn unlisted_root(&mut self, path: &str, errno: Errno) -> Result<(), Errno> {
        self.events.push(format!("unlisted {path} {errno:?}"));
        Ok(())
    }
}

fn tree() -> Tree {
    Tree::new()
        .dir("/r")
        .file("/r/a", b"a")
        .dir("/r/d")
        .file("/r/d/x", b"x")
        .link("/r/l", "/elsewhere")
        .dir("/elsewhere")
        .file("/elsewhere/kept", b"k")
}

fn walked(tree: &Tree) -> Vec<String> {
    let root = tree.open_dir("/r", false).expect("the root opens");
    let mut seen = Seen::default();
    walk(&root, "/r", &mut seen).expect("the walk ends");
    seen.events
}

#[test]
fn a_walk_visits_each_entry_before_descending_and_leaves_after_its_entries() {
    assert_eq!(
        walked(&tree()),
        [
            "enter /r/a File",
            "enter /r/d Directory",
            "enter /r/d/x File",
            "leave /r/d Directory",
            "enter /r/l Link",
        ]
    );
}

#[test]
fn a_directory_swapped_for_a_link_before_its_descent_is_never_entered() {
    let tree = tree();
    tree.before_line(
        "open_dir /r/d",
        alloc::boxed::Box::new(|tree: &Tree| tree.relink("/r/d", "/elsewhere")),
    );
    let events = walked(&tree);
    assert_eq!(
        events,
        [
            "enter /r/a File",
            "enter /r/d Directory",
            "refused /r/d NotADirectory",
            "enter /r/l Link",
        ]
    );
    assert!(
        tree.calls().iter().all(|call| !call.contains("/elsewhere")),
        "nothing reached what the link names: {:?}",
        tree.calls()
    );
}

#[test]
fn a_directory_whose_entries_will_not_list_is_refused_and_never_left() {
    let tree = tree().refuse("entries /r/d", Errno::PermissionDenied);
    assert_eq!(
        walked(&tree),
        [
            "enter /r/a File",
            "enter /r/d Directory",
            "refused /r/d PermissionDenied",
            "enter /r/l Link",
        ]
    );
}

#[test]
fn a_root_whose_entries_will_not_list_is_said_once() {
    let tree = tree().refuse("entries /r", Errno::PermissionDenied);
    assert_eq!(walked(&tree), ["unlisted /r PermissionDenied"]);
}

#[test]
fn a_visitor_that_skips_a_directory_keeps_the_walk_out_of_it() {
    struct Shallow(Vec<String>);
    impl<D: Dir> Visitor<D> for Shallow {
        type Error = Errno;
        fn enter(&mut self, _: &D, entry: &Entry, path: &str) -> Result<Step<D>, Errno> {
            self.0.push(String::from(path));
            Ok(if entry.name == "d" || entry.kind != Kind::Directory {
                Step::Skip
            } else {
                Step::Descend
            })
        }
        fn leave(&mut self, _: &D, _: &Entry, _: &str, _: &D) -> Result<(), Errno> {
            Err(Errno::DeviceFault)
        }
        fn refused(&mut self, _: &D, _: &Entry, _: &str, errno: Errno) -> Result<(), Errno> {
            Err(errno)
        }
        fn unlisted_root(&mut self, _: &str, errno: Errno) -> Result<(), Errno> {
            Err(errno)
        }
    }
    let tree = tree();
    let root = tree.open_dir("/r", false).expect("the root opens");
    let mut shallow = Shallow(Vec::new());
    assert_eq!(walk(&root, "/r", &mut shallow), Ok(()));
    assert_eq!(shallow.0, ["/r/a", "/r/d", "/r/l"]);
}

#[test]
fn every_descent_ends_in_one_leave_or_one_refusal_whatever_was_listed() {
    let tree = tree();
    // Listed as a file, a directory by the time the visitor answers.
    tree.before_line(
        "open_dir /r/a",
        alloc::boxed::Box::new(|tree: &Tree| {
            tree.remove("/r/a");
            let made = tree
                .open_dir("/r", false)
                .and_then(|r| r.make_dir("a", 0o755));
            assert!(made.is_ok());
        }),
    );
    let root = tree.open_dir("/r", false).expect("the root opens");
    let mut seen = Seen {
        everything: true,
        ..Seen::default()
    };
    walk(&root, "/r", &mut seen).expect("the walk ends");
    assert_eq!(
        seen.events,
        [
            "enter /r/a File",
            "leave /r/a Directory",
            "enter /r/d Directory",
            "enter /r/d/x File",
            "refused /r/d/x NotADirectory",
            "leave /r/d Directory",
            "enter /r/l Link",
            "refused /r/l NotADirectory",
        ]
    );
}

/// What a cursor reaches, an event a line, `between` run before each step.
fn reached<R: core::borrow::Borrow<TreeDir>>(
    mut cursor: Cursor<TreeDir, R>,
    mut between: impl FnMut(usize),
) -> Vec<String> {
    let mut events = Vec::new();
    loop {
        between(events.len());
        let descend = match cursor.reach() {
            None => return events,
            Some(Reached::Entry { entry, path, .. }) => {
                events.push(format!("entry {path}"));
                entry.kind == Kind::Directory
            }
            Some(Reached::Left { path, held, .. }) => {
                let names = held.entries().map(|entries| entries.len());
                events.push(format!("left {path} {names:?}"));
                false
            }
        };
        if descend {
            assert_eq!(cursor.descend(), Ok(()));
        }
    }
}

#[test]
fn a_cursor_keeps_its_place_and_its_directories_between_calls() {
    let tree = tree();
    let owned = reached(
        Cursor::new(tree.open_dir("/r", false).expect("opens"), "/r").expect("lists"),
        |_| {},
    );
    let root = tree.open_dir("/r", false).expect("opens");
    let borrowed = reached(Cursor::new(&root, "/r").expect("lists"), |_| {});
    assert_eq!(owned, borrowed);
    assert_eq!(
        owned,
        [
            "entry /r/a",
            "entry /r/d",
            "entry /r/d/x",
            "left /r/d Ok(1)",
            "entry /r/l",
        ]
    );
    // Renamed once the walk is inside it, the directory is still the one
    // listed and left, known by the name it was reached by.
    let renamed = reached(Cursor::new(&root, "/r").expect("lists"), |seen| {
        if seen == 3 {
            tree.rename("/r/d", "/r/e");
            tree.write("/r/e/y", b"y");
        }
    });
    assert_eq!(renamed[3], "left /r/d Ok(2)");
}

#[test]
fn a_cursor_descends_only_from_an_entry_and_never_into_a_directory_it_is_inside() {
    let tree = tree();
    let root = tree.open_dir("/r", false).expect("opens");
    let mut cursor = Cursor::<TreeDir, _>::new(&root, "/r").expect("lists");
    assert_eq!(cursor.descend(), Err(Errno::OutOfRange), "at no entry yet");
    let looped = Tree::new()
        .dir("/r/d")
        .hard_link("/r/d/up", "/r")
        .file("/r/d/f", b"f");
    let root = looped.open_dir("/r", false).expect("opens");
    let mut seen = Seen::default();
    walk(&root, "/r", &mut seen).expect("the walk ends");
    assert_eq!(
        seen.events,
        [
            "enter /r/d Directory",
            "enter /r/d/up Directory",
            "enter /r/d/up/d Directory",
            "refused /r/d/up/d LinkLoop",
            "leave /r/d/up Directory",
            "enter /r/d/f File",
            "leave /r/d Directory",
        ]
    );
}

#[test]
fn a_visitor_descends_the_very_directory_it_opened() {
    struct Holding(Vec<String>);
    impl Visitor<TreeDir> for Holding {
        type Error = Errno;
        fn enter(
            &mut self,
            dir: &TreeDir,
            entry: &Entry,
            path: &str,
        ) -> Result<Step<TreeDir>, Errno> {
            self.0.push(String::from(path));
            if entry.name != "d" {
                return Ok(Step::Skip);
            }
            let held = dir.open_dir(&entry.name)?;
            Ok(Step::Into(held))
        }
        fn leave(&mut self, _: &TreeDir, _: &Entry, path: &str, _: &TreeDir) -> Result<(), Errno> {
            self.0.push(format!("leave {path}"));
            Ok(())
        }
        fn refused(&mut self, _: &TreeDir, _: &Entry, _: &str, errno: Errno) -> Result<(), Errno> {
            Err(errno)
        }
        fn unlisted_root(&mut self, _: &str, errno: Errno) -> Result<(), Errno> {
            Err(errno)
        }
    }
    let tree = tree().dir("/swapped").file("/swapped/other", b"o");
    // Swapped once the visitor holds it: the walk lists what it holds.
    tree.before_line(
        "entries /r/d",
        alloc::boxed::Box::new(|tree: &Tree| {
            tree.rename("/r/d", "/r/old");
            tree.rename("/swapped", "/r/d");
        }),
    );
    let root = tree.open_dir("/r", false).expect("opens");
    let mut holding = Holding(Vec::new());
    walk(&root, "/r", &mut holding).expect("the walk ends");
    assert_eq!(holding.0, ["/r/a", "/r/d", "/r/d/x", "leave /r/d", "/r/l"]);
}

#[test]
fn an_operand_is_opened_through_its_final_link() {
    let tree = tree()
        .link("/r/to-d", "d")
        .link("/r/to-a", "a")
        .link("/r/dangling", "nowhere");
    let held_dir = |path| match open_operand(&tree, path) {
        Ok(Operand::Directory(dir)) => dir.stat().map(|stat| stat.id).ok(),
        _ => None,
    };
    assert_eq!(held_dir("/r/to-d"), tree.id("/r/d"));
    assert_eq!(held_dir("/r/d"), tree.id("/r/d"));
    let held_node = |path| match open_operand(&tree, path) {
        Ok(Operand::Node(node)) => node.stat().map(|stat| (stat.id, stat.kind)).ok(),
        _ => None,
    };
    let a = tree.id("/r/a").map(|id| (id, FileKind::Regular));
    assert_eq!(held_node("/r/to-a"), a);
    assert_eq!(held_node("/r/a"), a);
    assert!(matches!(
        open_operand(&tree, "/r/dangling"),
        Err(Unopened::Dangling)
    ));
    assert!(matches!(
        open_operand(&tree, "/r/missing"),
        Err(Unopened::Refused(Errno::NotFound))
    ));
}

#[test]
fn a_kind_is_read_from_the_node_not_what_it_names() {
    assert_eq!(Kind::of(FileKind::Directory), Kind::Directory);
    assert_eq!(Kind::of(FileKind::Regular), Kind::File);
    assert_eq!(Kind::of(FileKind::Symlink), Kind::Link);
}
