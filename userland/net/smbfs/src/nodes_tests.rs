//! Unit tests for a mount's node map.

use tairix_abi::driver::filesystem::{NodeId, Unpinned};
use tairix_hash::{BuildSipHash13, HashSeed};
use tairix_smb::file::name::SharePath;

use super::{same, within, Nodes, NODES_MAX, ROOT};

fn nodes() -> Nodes {
    Nodes::new(&BuildSipHash13::with_seed(HashSeed::from_bytes([7; 16])))
}

fn path(text: &str) -> SharePath {
    SharePath::parse(text).expect("a path")
}

#[test]
fn a_path_keeps_its_id_until_another_node_takes_its_name() {
    let mut nodes = nodes();
    assert_eq!(nodes.name(&SharePath::root(), 0, true), Ok(ROOT));
    let file = nodes.name(&path("a\\b"), 41, false).expect("room");
    assert_ne!(file, ROOT);
    assert_eq!(nodes.name(&path("a\\b"), 41, false), Ok(file));
    assert_eq!(
        nodes.name(&path("a\\b"), 0, false),
        Ok(file),
        "no id to tell apart"
    );
    assert_eq!(nodes.get(file).map(|node| node.file_id), Some(41));
    let replaced = nodes.name(&path("a\\b"), 42, false).expect("room");
    assert_ne!(replaced, file, "another writer replaced it");
    assert!(nodes.get(file).is_none(), "the old id is stale");
    nodes.forget(replaced.raw());
    assert!(nodes.get(replaced).is_none());
    assert!(nodes.get(ROOT).is_some(), "the root is always named");
}

#[test]
fn the_least_recently_used_is_forgotten_and_a_pinned_node_never_is() {
    let mut nodes = nodes();
    let pinned = nodes.name(&path("held"), 1, false).expect("room");
    assert_eq!(nodes.pin(pinned), Ok(true));
    let first = nodes.name(&path("n0"), 0, false).expect("room");
    for at in 1..NODES_MAX {
        nodes
            .name(&path(&alloc::format!("n{at}")), 0, false)
            .expect("room");
    }
    // Naming it again makes it the most recently used.
    let recent = nodes.name(&path("n1"), 0, false).expect("room");
    nodes.name(&path("one more"), 0, false).expect("room");
    assert!(nodes.get(first).is_none(), "the least recently used went");
    assert!(nodes.get(recent).is_some());
    assert!(nodes.get(pinned).is_some(), "a held node stays");
    assert_eq!(nodes.unpin(pinned), Some(Unpinned::Released));
    assert_eq!(nodes.unpin(pinned), None, "no hold left");
}

#[test]
fn a_rename_moves_the_node_and_everything_beneath_it() {
    let mut nodes = nodes();
    let dir = nodes.name(&path("docs"), 1, true).expect("room");
    let child = nodes.name(&path("docs\\a.txt"), 2, false).expect("room");
    let deep = nodes
        .name(&path("docs\\sub\\b.txt"), 3, false)
        .expect("room");
    let beside = nodes.name(&path("docsets"), 4, false).expect("room");
    nodes
        .moved(&path("docs"), &path("archive\\docs"))
        .expect("room");
    assert_eq!(
        nodes.get(dir).map(|node| node.path),
        Some(path("archive\\docs"))
    );
    assert_eq!(
        nodes.get(child).map(|node| node.path),
        Some(path("archive\\docs\\a.txt"))
    );
    assert_eq!(
        nodes.get(deep).map(|node| node.path),
        Some(path("archive\\docs\\sub\\b.txt"))
    );
    assert_eq!(
        nodes.get(beside).map(|node| node.path),
        Some(path("docsets")),
        "a name sharing the prefix is not beneath it"
    );
    assert_eq!(
        nodes.name(&path("archive\\docs\\a.txt"), 2, false),
        Ok(child)
    );
}

#[test]
fn a_held_node_whose_name_went_is_freed_on_its_last_unpin() {
    let mut nodes = nodes();
    let file = nodes.name(&path("f"), 9, false).expect("room");
    assert_eq!(nodes.pin(file), Ok(true));
    assert_eq!(nodes.pin(file), Ok(true));
    assert_eq!(nodes.unpin(file), Some(Unpinned::Held));
    let other = nodes.name(&path("f"), 10, false).expect("room");
    assert_ne!(other, file);
    assert!(nodes.get(file).is_none(), "the replaced node is stale");
    assert_eq!(nodes.pin(file), Ok(false), "a stale node takes no new hold");
    assert_eq!(nodes.unpin(file), Some(Unpinned::Freed));
    assert_eq!(nodes.pin(NodeId::from_raw(999)), Ok(false), "unknown");
    let held = nodes.name(&path("g"), 11, false).expect("room");
    assert_eq!(nodes.pin(held), Ok(true));
    nodes.release_pins();
    assert_eq!(nodes.unpin(held), None);
    assert!(nodes.get(held).is_some(), "released, still named");
}

#[test]
fn a_name_in_either_case_is_one_node_kept_as_last_moved() {
    let mut nodes = nodes();
    let dir = nodes.name(&path("Docs"), 1, true).expect("room");
    let file = nodes
        .name(&path("Docs\\Notes.TXT"), 2, false)
        .expect("room");
    assert_eq!(nodes.name(&path("docs\\notes.txt"), 2, false), Ok(file));
    assert_eq!(nodes.at(&path("DOCS")).map(|node| node.file_id), Some(1));
    nodes.moved(&path("docs"), &path("Archive")).expect("room");
    assert_eq!(
        nodes.get(file).map(|node| node.path),
        Some(path("Archive\\Notes.TXT")),
        "the rest keeps its spelling"
    );
    nodes
        .moved(&path("archive"), &path("ARCHIVE"))
        .expect("room");
    assert_eq!(nodes.get(dir).map(|node| node.path), Some(path("ARCHIVE")));
    assert_eq!(
        nodes
            .at(&path("archive\\notes.txt"))
            .map(|node| node.file_id),
        Some(2)
    );
    nodes.forget_at(&path("ARCHIVE\\NOTES.txt"));
    assert!(nodes.get(file).is_none());
}

#[test]
fn a_rename_over_a_named_node_forgets_it() {
    let mut nodes = nodes();
    let moved = nodes.name(&path("a"), 1, false).expect("room");
    let replaced = nodes.name(&path("b"), 2, false).expect("room");
    nodes.moved(&path("a"), &path("B")).expect("room");
    assert!(nodes.get(replaced).is_none(), "the replaced node is stale");
    assert_eq!(nodes.name(&path("b"), 1, false), Ok(moved));
    nodes.moved(&path("gone"), &path("b")).expect("room");
    assert!(
        nodes.get(moved).is_none(),
        "a move of what no one holds still replaces"
    );
}

#[test]
fn a_path_lies_within_another_only_beneath_a_whole_name() {
    assert!(within(&path("Docs\\a"), &path("docs")));
    assert!(within(&path("docs"), &path("DOCS")));
    assert!(within(&path("docs"), &SharePath::root()));
    assert!(!within(&path("docsets"), &path("docs")));
    assert!(!within(&path("doc"), &path("docs")));
    assert!(same(&path("A\\b"), &path("a\\B")));
    assert!(!same(&path("a"), &path("a\\b")));
}
