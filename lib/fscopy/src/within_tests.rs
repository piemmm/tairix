use tairix_abi::{Errno, FileId};

use super::{landing, Descent, Landing, Unresolved};
use crate::fixture::{Handle, Tree};
use crate::tree::Roots;

const fn id(node: u64) -> FileId {
    FileId {
        volume: [7; 16],
        node,
    }
}

fn tree() -> Tree {
    Tree::new()
        .dir("/a/x/y")
        .dir("/b")
        .dir("/ab")
        .link("/b/to-a", "/a")
        .dir("/a/mnt")
        .volume("/a/mnt")
}

fn open(tree: &Tree, path: &str) -> Handle {
    tree.open_dir(path, true).expect("the directory opens")
}

fn landed(tree: &Tree, source: &str, dir: &str) -> Result<Landing, Unresolved> {
    landing(&open(tree, source), &open(tree, dir))
}

#[test]
fn a_climb_ends_at_a_root_with_no_identity() {
    let tree = Tree::new().dir("/s").dir("/d/e").unidentified("/");
    let before = tree.calls().len();
    assert_eq!(landed(&tree, "/s", "/d/e"), Ok(Landing::Apart));
    assert!(tree.calls().len() - before < 16, "{:?}", tree.calls());
}

#[test]
fn a_directory_beside_the_source_is_apart() {
    let tree = tree();
    assert_eq!(landed(&tree, "/a", "/b"), Ok(Landing::Apart));
    assert_eq!(landed(&tree, "/a", "/ab"), Ok(Landing::Apart));
    assert_eq!(landed(&tree, "/a/x", "/a"), Ok(Landing::Apart));
}

#[test]
fn a_directory_inside_the_source_is_within_however_deep() {
    let tree = tree();
    assert_eq!(landed(&tree, "/a", "/a/x"), Ok(Landing::Within));
    assert_eq!(landed(&tree, "/a", "/a/x/y"), Ok(Landing::Within));
}

#[test]
fn the_source_itself_is_the_same() {
    assert_eq!(landed(&tree(), "/a", "/a"), Ok(Landing::Same));
}

#[test]
fn a_link_on_the_way_cannot_hide_the_source() {
    let tree = tree();
    assert_eq!(landed(&tree, "/a", "/b/to-a/x"), Ok(Landing::Within));
    assert_eq!(landed(&tree, "/a", "/b/to-a"), Ok(Landing::Same));
    assert_eq!(landed(&tree, "/a", "/b/../a/x/y"), Ok(Landing::Within));
}

#[test]
fn a_volume_attached_inside_the_source_is_within_it() {
    let tree = tree().dir("/a/mnt/in");
    assert_eq!(landed(&tree, "/a", "/a/mnt/in"), Ok(Landing::Within));
    assert_eq!(landed(&tree, "/a/mnt", "/b"), Ok(Landing::Apart));
}

#[test]
fn a_climb_that_fails_is_the_targets_and_a_source_unknown_is_the_sources() {
    let tree = tree().refuse("open_dir /a/x/..", Errno::PermissionDenied);
    assert_eq!(
        landed(&tree, "/b", "/a/x/y"),
        Err(Unresolved::Target(Errno::PermissionDenied))
    );
    let tree = self::tree().refuse("stat /b", Errno::DeviceFault);
    let failure = landed(&tree, "/b", "/a");
    assert_eq!(failure, Err(Unresolved::Source(Errno::DeviceFault)));
    assert_eq!(
        failure.map_err(|unresolved| unresolved.at("/b", "/a")),
        Err(("/b", Errno::DeviceFault))
    );
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
