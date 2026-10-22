use alloc::collections::BTreeMap;
use alloc::string::String;

use tairix_abi::{Errno, FileId};

use super::{landing, Ancestry, Descent, Landing};

const fn id(node: u64) -> FileId {
    FileId {
        volume: [7; 16],
        node,
    }
}

/// Spellings resolved by table (anything else resolves to itself) and the
/// identity of every directory that exists.
#[derive(Default)]
struct Tree {
    spelled: BTreeMap<&'static str, &'static str>,
    nodes: BTreeMap<&'static str, FileId>,
}

impl Tree {
    fn with(mut self, path: &'static str, node: u64) -> Self {
        self.nodes.insert(path, id(node));
        self
    }

    fn spelling(mut self, path: &'static str, canonical: &'static str) -> Self {
        self.spelled.insert(path, canonical);
        self
    }
}

impl Ancestry for Tree {
    fn canonical(&self, path: &str) -> Result<String, Errno> {
        Ok(String::from(
            self.spelled.get(path).copied().unwrap_or(path),
        ))
    }

    fn identity(&self, path: &str) -> Result<FileId, Errno> {
        self.nodes.get(path).copied().ok_or(Errno::NotFound)
    }
}

fn tree() -> Tree {
    Tree::default()
        .with("/", 1)
        .with("/a", 2)
        .with("/a/x", 3)
        .with("/b", 4)
        .with("/Users", 5)
        .with("/Users/root", 6)
        .with("/Users/root/Home", 7)
}

#[test]
fn a_target_beside_the_source_is_apart() {
    assert_eq!(landing(&tree(), "/a", id(2), "/b/a"), Ok(Landing::Apart));
    assert_eq!(landing(&tree(), "/a", id(2), "/ab"), Ok(Landing::Apart));
}

#[test]
fn a_target_inside_the_source_is_within_however_deep() {
    assert_eq!(landing(&tree(), "/a", id(2), "/a/b"), Ok(Landing::Within));
    assert_eq!(landing(&tree(), "/a", id(2), "/a/x/y"), Ok(Landing::Within));
}

#[test]
fn the_source_itself_is_the_same() {
    assert_eq!(landing(&tree(), "/a", id(2), "/a"), Ok(Landing::Same));
}

#[test]
fn a_link_or_a_climb_on_the_way_cannot_hide_the_source() {
    let tree = tree()
        .spelling("/b/to-a/y", "/a/y")
        .spelling("/b/../a/y", "/a/y");
    assert_eq!(
        landing(&tree, "/a", id(2), "/b/to-a/y"),
        Ok(Landing::Within)
    );
    assert_eq!(
        landing(&tree, "/a", id(2), "/b/../a/y"),
        Ok(Landing::Within)
    );
}

#[test]
fn a_second_spelling_of_the_source_is_found_by_its_identity() {
    let tree = tree()
        .with("Home:/", 7)
        .with("Home:/docs", 8)
        .with("/Users/root/Home/docs", 8);
    assert_eq!(
        landing(&tree, "Home:/docs", id(8), "/Users/root/Home/docs/copy"),
        Ok(Landing::Within)
    );
    assert_eq!(
        landing(&tree, "Home:/docs", id(8), "/Users/root/Home/docs"),
        Ok(Landing::Same)
    );
}

#[test]
fn without_an_identity_only_the_resolved_paths_decide() {
    let tree = tree()
        .with("Home:/docs", 8)
        .with("/Users/root/Home/docs", 8);
    assert_eq!(
        landing(
            &tree,
            "Home:/docs",
            FileId::NONE,
            "/Users/root/Home/docs/copy"
        ),
        Ok(Landing::Apart)
    );
    assert_eq!(
        landing(&tree, "/a", FileId::NONE, "/a/b"),
        Ok(Landing::Within)
    );
}

#[test]
fn a_resolution_that_fails_is_the_answer() {
    struct Broken;
    impl Ancestry for Broken {
        fn canonical(&self, _: &str) -> Result<String, Errno> {
            Err(Errno::LinkLoop)
        }
        fn identity(&self, _: &str) -> Result<FileId, Errno> {
            Ok(id(2))
        }
    }
    assert_eq!(landing(&Broken, "/a", id(2), "/b"), Err(Errno::LinkLoop));
    let garbled = tree().spelling("/b/x", "not//a path");
    assert_eq!(
        landing(&garbled, "/a", id(2), "/b/x"),
        Err(Errno::OutOfRange)
    );
    let gone = Tree::default().with("/", 1).with("/a", 2);
    assert_eq!(landing(&gone, "/a", id(2), "/b/x"), Err(Errno::NotFound));
}

#[test]
fn a_walk_refuses_each_directory_it_is_inside_and_no_other() {
    let mut descent = Descent::default();
    descent.enter(id(2), id(20)).expect("room for a level");
    descent.enter(id(3), id(30)).expect("room for a level");
    assert!(descent.refuses(id(2)));
    assert!(descent.refuses(id(30)));
    assert!(!descent.refuses(id(4)));
    assert!(!descent.refuses(FileId::NONE));
    descent.leave();
    assert!(!descent.refuses(id(3)));
    assert!(!descent.refuses(id(30)));
    assert!(descent.refuses(id(20)));
    descent.leave();
    descent.leave();
    assert!(!descent.refuses(id(2)));
}
