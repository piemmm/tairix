//! `Scrap`'s tree: bounded, its nodes living while named or held.

use alloc::vec::Vec;

use tairix_abi::{Errno, RenameFlags, UnlinkFlags};
use tairix_fscopy::tree::{Creation, Dir, FileAccess, Node as _, WriteAt};
use tairix_fscopy::{Source, Target};

use super::{bound, Tree};

#[test]
fn a_write_past_the_bound_is_cut_short_then_refused() {
    let tree = Tree::new(16);
    let root = tree.root();
    let mut file = root
        .open_reached("f", FileAccess::Both, Creation::New, 0o666)
        .expect("made");
    assert_eq!(
        file.write_at(0, b"0123456789abcdef"),
        Ok(15),
        "the name takes one byte of sixteen"
    );
    assert_eq!(file.write_at(15, b"!"), Err(Errno::NoSpace));
    assert_eq!(file.set_len(100), Err(Errno::NoSpace));
    let mut read = [0; 15];
    assert_eq!(file.read_at(0, &mut read), Ok(15));
    assert_eq!(root.volume_stats().map(|stats| stats.free_blocks), Ok(0));
    assert_eq!(bound(8 << 20), 1 << 20, "an eighth of the session");
}

#[test]
fn a_deleted_file_keeps_its_bytes_while_it_is_held() {
    let tree = Tree::new(1024);
    let root = tree.root();
    let mut file = root
        .open_reached("f", FileAccess::Both, Creation::New, 0o666)
        .expect("made");
    file.write_all_at(0, b"kept").expect("written");
    root.unlink("f", UnlinkFlags::empty(), None)
        .expect("deleted");
    assert_eq!(root.entries(usize::MAX).map(|entries| entries.len()), Ok(0));
    let mut read = [0; 4];
    assert_eq!(file.read_at(0, &mut read), Ok(4));
    assert_eq!(&read, b"kept");
    let held = root.volume_stats().expect("its space").free_blocks;
    drop(file);
    assert!(
        root.volume_stats().expect("its space").free_blocks >= held,
        "its bytes given back"
    );
}

#[test]
fn a_directory_is_renamed_but_never_into_itself() {
    let tree = Tree::new(1024);
    let root = tree.root();
    let outer = root.make_dir("outer", 0o777).expect("made");
    outer.make_dir("inner", 0o777).expect("made");
    let inner = outer.open_dir("inner").expect("held");
    assert_eq!(
        root.rename("outer", &inner, "loop", RenameFlags::NO_REPLACE, None),
        Err(Errno::OutOfRange)
    );
    assert_eq!(
        root.unlink("outer", UnlinkFlags::DIRECTORY, None),
        Err(Errno::NotEmpty)
    );
    root.rename("outer", &root, "moved", RenameFlags::NO_REPLACE, None)
        .expect("renamed");
    let names: Vec<_> = root
        .entries(usize::MAX)
        .expect("listed")
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    assert_eq!(names, ["moved"]);
}

#[test]
fn a_nodes_attributes_are_kept_and_counted() {
    let tree = Tree::new(64);
    let root = tree.root();
    root.make_dir("d", 0o777).expect("made");
    let node = root.open_node("d").expect("held");
    node.attr_set("acorn.attr", b"WR/").expect("kept");
    assert_eq!(node.attr_value("acorn.attr"), Ok(b"WR/".to_vec()));
    assert_eq!(
        node.attr_set("big", &[0; 64]),
        Err(Errno::NoSpace),
        "past the bound"
    );
    node.attr_remove("acorn.attr").expect("removed");
    assert_eq!(node.attr_value("acorn.attr"), Err(Errno::NotFound));
}
