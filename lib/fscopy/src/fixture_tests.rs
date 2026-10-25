use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::{Errno, NodeExpect, RenameFlags, UnlinkFlags};

use super::{resolve, Tree, USER};
use crate::data::Target;
use crate::tree::{Dir, Kind, Node, Roots};

#[test]
fn a_held_directory_is_its_node_wherever_its_name_goes() {
    let tree = Tree::new().dir("/a").file("/a/f", b"held");
    let held = tree.open_dir("/a", false).expect("opens");
    tree.rename("/a", "/b");
    tree.relink("/a", "/b");
    let names: Vec<_> = held
        .entries()
        .expect("lists")
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    assert_eq!(names, ["f"]);
    assert_eq!(held.stat().map(|stat| stat.id).ok(), tree.id("/b"));
}

#[test]
fn a_hook_runs_once_immediately_before_its_call() {
    let tree = Tree::new().dir("/d").file("/d/f", b"x");
    tree.before_call(1, Box::new(|tree: &Tree| tree.rename("/d/f", "/d/g")));
    let dir = tree.open_dir("/d", false).expect("opens");
    assert_eq!(dir.open_node("f").err(), Some(Errno::NotFound));
    assert!(dir.open_node("g").is_ok());
    assert_eq!(
        tree.calls(),
        ["open_dir /d", "open_node /d/f", "open_node /d/g"]
    );
}

#[test]
fn a_create_takes_the_umask_and_a_link_does_not() {
    let tree = Tree::new().dir("/d").with_umask(0o027);
    let dir = tree.open_dir("/d", false).expect("opens");
    dir.create_file("f", 0o666).expect("made");
    dir.make_dir("s", 0o777).expect("made");
    dir.symlink(b"f", "l").expect("made");
    assert_eq!(tree.mode("/d/f"), Some(0o640));
    assert_eq!(tree.mode("/d/s"), Some(0o750));
    assert_eq!(tree.mode("/d/l"), Some(0o777));
    assert_eq!(tree.owner("/d/f"), Some(USER));
}

#[test]
fn an_expectation_that_no_longer_holds_is_stale_and_changes_nothing() {
    let tree = Tree::new().dir("/d").file("/d/f", b"one");
    let dir = tree.open_dir("/d", false).expect("opens");
    let before = dir.open_node("f").and_then(|f| f.stat()).expect("stat");
    tree.write("/d/f", b"one, and more");
    assert_eq!(
        dir.unlink(
            "f",
            UnlinkFlags::EXPECT_UNCHANGED,
            Some(&NodeExpect::of(&before))
        ),
        Err(Errno::Stale)
    );
    tree.remove("/d/f");
    tree.write("/d/f", b"another");
    assert_eq!(
        dir.rename(
            "f",
            &dir,
            "g",
            RenameFlags::EXPECT_NODE,
            Some(&NodeExpect::of_node(before.id))
        ),
        Err(Errno::Stale)
    );
    assert_eq!(tree.bytes("/d/f").as_deref(), Some(&b"another"[..]));
    assert!(!tree.exists("/d/g"));
}

#[test]
fn a_link_is_followed_only_where_the_call_asks() {
    let tree = Tree::new()
        .dir("/d")
        .file("/f", b"x")
        .link("/to-d", "/d")
        .link("/to-f", "/f")
        .link("/dangling", "/nowhere");
    let root = tree.open_dir("/", false).expect("opens");
    assert_eq!(root.open_dir("to-d").err(), Some(Errno::NotADirectory));
    assert_eq!(root.open_file("to-f", false).err(), Some(Errno::LinkLoop));
    assert!(tree.open_dir("/to-d", true).is_ok());
    assert_eq!(
        tree.open_dir("/to-d", false).err(),
        Some(Errno::NotADirectory)
    );
    assert_eq!(tree.open_dir("/f", true).err(), Some(Errno::NotADirectory));
    assert_eq!(
        tree.open_dir("/dangling", true).err(),
        Some(Errno::NotFound)
    );
    assert_eq!(
        tree.open_node("/to-f", false)
            .and_then(|node| node.stat())
            .map(|stat| stat.kind),
        Ok(tairix_abi::FileKind::Symlink)
    );
}

#[test]
fn a_rename_across_volumes_is_refused() {
    let tree = Tree::new().file("/a", b"x").volume("/v");
    let (from, name) = tree.parent("/a").expect("opens");
    let to = tree.open_dir("/v", false).expect("opens");
    assert_eq!(
        from.rename(&name, &to, "a", RenameFlags::empty(), None),
        Err(Errno::CrossVolume)
    );
    assert_eq!(tree.kind("/a"), Some(Kind::File));
}

#[test]
fn a_refused_call_is_counted_and_listed_and_changes_nothing() {
    let tree = Tree::new()
        .dir("/d")
        .file("/d/a", b"x")
        .refuse("unlink /d/a", Errno::PermissionDenied)
        .refuse("write /d/b", Errno::NoSpace);
    let dir = tree.open_dir("/d", false).expect("opens");
    assert_eq!(
        dir.unlink("a", UnlinkFlags::empty(), None),
        Err(Errno::PermissionDenied)
    );
    assert!(tree.exists("/d/a"));
    assert_eq!(tree.calls().last().map(String::as_str), Some("unlink /d/a"));
    let mut b = dir.create_file("b", 0o600).expect("made");
    assert_eq!(b.write_all_at(0, b"bytes"), Err(Errno::NoSpace));
}

#[test]
fn checked_permissions_keep_a_name_out_of_a_directory_its_owner_may_not_write() {
    let open = Tree::new().dir("/d").with_mode("/d", 0o500);
    let held = open.open_dir("/d", false).expect("opens");
    assert!(held.create_file("f", 0o600).is_ok(), "unchecked by default");
    let checked = Tree::new()
        .dir("/d")
        .with_mode("/d", 0o500)
        .checking_permissions();
    let held = checked.open_dir("/d", false).expect("opens");
    assert_eq!(
        held.create_file("f", 0o600).err(),
        Some(Errno::PermissionDenied)
    );
    held.set_mode(0o700).expect("its owner may change its mode");
    assert!(held.create_file("f", 0o600).is_ok());
}

#[test]
fn only_a_privileged_caller_gives_a_node_away() {
    let tree = Tree::new().file("/f", b"x").with_mode("/f", 0o4755);
    let node = tree.open_node("/f", false).expect("opens");
    assert_eq!(node.set_owner(7, 7), Err(Errno::PermissionDenied));
    let tree = tree.privileged();
    node.set_owner(7, 7).expect("given away");
    assert_eq!(tree.owner("/f"), Some((7, 7)));
    assert_eq!(tree.mode("/f"), Some(0o755), "the set-ID bit went with it");
}

fn links(path: &str) -> Option<String> {
    match path {
        "/up" => Some(String::from("..")),
        "/a/rel" => Some(String::from("b/c")),
        "/abs" => Some(String::from("/a")),
        "/loop" => Some(String::from("/loop")),
        _ => None,
    }
}

#[test]
fn each_link_and_dot_dot_on_the_way_is_resolved() {
    assert_eq!(resolve("/abs/rel/d", links), Ok(String::from("/a/b/c/d")));
    assert_eq!(resolve("/a/../up/x", links), Ok(String::from("/x")));
    assert_eq!(resolve("//a/./b/", links), Ok(String::from("/a/b")));
    assert_eq!(resolve("/..", links), Ok(String::from("/")));
}

#[test]
fn a_cycle_of_links_is_cut_off() {
    assert_eq!(resolve("/loop/x", links), Err(Errno::LinkLoop));
}
