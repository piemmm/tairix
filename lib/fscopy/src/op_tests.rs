use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::Errno;

use super::{Conflict, Decision, Named, OpError, Operation, Progress, Taken, Verb};
use crate::fixture::{staged, Handle, Tree, USER};
use crate::tree::Kind;

fn named(tree: &Tree, path: &str) -> Named<Handle> {
    Named::parent(tree, path).expect("its directory opens")
}

fn copying(tree: &Tree, from: &str, to: &str, taken: Taken) -> Operation<Handle> {
    Operation::transfer(
        Verb::Copy,
        named(tree, from),
        named(tree, to),
        taken,
        staged,
    )
}

fn moving(tree: &Tree, from: &str, to: &str, taken: Taken) -> Operation<Handle> {
    Operation::transfer(
        Verb::Move,
        named(tree, from),
        named(tree, to),
        taken,
        staged,
    )
}

fn removing(tree: &Tree, path: &str) -> Operation<Handle> {
    Operation::remove(named(tree, path))
}

/// Advance `op` until it ends or asks.
fn run(op: &mut Operation<Handle>) -> Progress {
    let mut buf = Vec::new();
    loop {
        match op.advance(usize::MAX, &mut buf) {
            Progress::Yielded => {}
            progress => return progress,
        }
    }
}

/// Advance `op` to its end, skipping whatever it asks about.
fn run_out(op: &mut Operation<Handle>) -> Progress {
    loop {
        match run(op) {
            Progress::Paused => op.resolve(Decision::Skip),
            progress => return progress,
        }
    }
}

/// Advance `op` to its end, overwriting whatever it asks about.
fn run_over(op: &mut Operation<Handle>) -> Progress {
    loop {
        match run(op) {
            Progress::Paused => op.resolve(Decision::Overwrite),
            progress => return progress,
        }
    }
}

fn index_of(tree: &Tree, line: &str) -> usize {
    tree.calls()
        .iter()
        .position(|call| call == line)
        .unwrap_or_else(|| panic!("{line} was never called: {:?}", tree.calls()))
}

#[test]
fn a_copy_keeps_a_files_holes_and_attributes() {
    let tree = Tree::new()
        .sparse(
            "/src/image",
            3 << 20,
            &[(0, &[7; 5000]), (2 << 20, &[9; 100])],
        )
        .attr("/src/image", "acorn.filetype", b"ffd")
        .dir("/dst");
    let mut op = copying(&tree, "/src/image", "/dst/image", Taken::Ask);
    assert_eq!(run(&mut op), Progress::Done);
    assert_eq!(tree.bytes("/dst/image"), tree.bytes("/src/image"));
    assert_eq!(
        tree.written("/dst/image"),
        5100,
        "only the stored runs are written"
    );
    assert_eq!(
        tree.attr_of("/dst/image", "acorn.filetype").as_deref(),
        Some(&b"ffd"[..])
    );
    assert_eq!(op.report().done, 1);
    assert_eq!(op.report().bytes, 3 << 20);
    assert!(op.omissions().is_empty());
}

#[test]
fn a_copy_reproduces_a_tree_and_counts_each_entry_it_made() {
    let tree = Tree::new()
        .file("/src/a", b"a")
        .file("/src/sub/b", b"bb")
        .link("/src/sub/l", "../a")
        .attr("/src/sub", "user.note", b"kept")
        .dir("/dst");
    let mut op = copying(&tree, "/src", "/dst/src", Taken::Ask);
    assert_eq!(run(&mut op), Progress::Done);
    assert_eq!(tree.bytes("/dst/src/a").as_deref(), Some(&b"a"[..]));
    assert_eq!(tree.bytes("/dst/src/sub/b").as_deref(), Some(&b"bb"[..]));
    assert_eq!(tree.link_target("/dst/src/sub/l").as_deref(), Some("../a"));
    assert_eq!(
        tree.attr_of("/dst/src/sub", "user.note").as_deref(),
        Some(&b"kept"[..]),
        "a directory made carries its source's attributes"
    );
    assert_eq!(op.report().done, 5, "two directories, two files, a link");
    assert_eq!(tree.bytes("/src/sub/b").as_deref(), Some(&b"bb"[..]));
}

#[test]
fn a_copy_gives_what_it_makes_its_sources_mode_less_the_umask() {
    let tree = Tree::new()
        .file("/src/private", b"p")
        .file("/src/tool", b"t")
        .with_mode("/src", 0o700)
        .with_mode("/src/private", 0o600)
        .with_mode("/src/tool", 0o4755)
        .with_umask(0o027);
    let mut op = copying(&tree, "/src", "/dst", Taken::Ask);
    assert_eq!(run(&mut op), Progress::Done);
    assert_eq!(tree.mode("/dst"), Some(0o700));
    assert_eq!(tree.mode("/dst/private"), Some(0o600));
    assert_eq!(
        tree.mode("/dst/tool"),
        Some(0o750),
        "its set-ID bit dropped"
    );
}

/// D1485: a folder its owner may not write is filled whole, and its copy
/// ends as its source less the umask once its entries are in.
#[test]
fn a_read_only_folder_is_filled_and_ends_as_its_source_less_the_umask() {
    let tree = Tree::new()
        .file("/ro/f", b"f")
        .file("/ro/sub/g", b"g")
        .with_mode("/ro/sub", 0o500)
        .with_mode("/ro", 0o500)
        .checking_permissions();
    let mut op = copying(&tree, "/ro", "/copy", Taken::Refuse);
    assert_eq!(run(&mut op), Progress::Done);
    assert_eq!(tree.bytes("/copy/f").as_deref(), Some(&b"f"[..]));
    assert_eq!(tree.bytes("/copy/sub/g").as_deref(), Some(&b"g"[..]));
    assert_eq!(tree.mode("/copy"), Some(0o500));
    assert_eq!(tree.mode("/copy/sub"), Some(0o500));
    let filled = tree.fill_modes();
    assert!(
        filled
            .iter()
            .filter(|(path, _)| path == "/copy" || path == "/copy/sub")
            .all(|(_, mode)| mode & 0o700 == 0o700),
        "filled while its owner could write it: {filled:?}"
    );
}

#[test]
fn a_umask_withholding_the_owners_write_still_fills_the_copy() {
    let tree = Tree::new()
        .file("/src/f", b"x")
        .with_umask(0o277)
        .checking_permissions();
    let mut op = copying(&tree, "/src", "/dst", Taken::Ask);
    assert_eq!(run(&mut op), Progress::Done);
    assert_eq!(tree.bytes("/dst/f").as_deref(), Some(&b"x"[..]));
    assert_eq!(tree.mode("/dst/f"), Some(0o400));
    assert_eq!(tree.mode("/dst"), Some(0o500));
}

#[test]
fn a_copied_directory_keeps_an_inherited_setgid_and_its_sources_sticky_bit() {
    let tree = Tree::new()
        .dir("/src")
        .dir("/shared")
        .with_mode("/src", 0o1750)
        .with_mode("/shared", 0o2770);
    let mut op = copying(&tree, "/src", "/shared/src", Taken::Ask);
    assert_eq!(run(&mut op), Progress::Done);
    assert_eq!(tree.mode("/shared/src"), Some(0o3750));
}

#[test]
fn a_merging_copy_asks_about_each_taken_leaf_alone() {
    let tree = Tree::new()
        .file("/src/one", b"new")
        .file("/src/two", b"2")
        .file("/dst/src/one", b"old");
    let mut op = copying(&tree, "/src", "/dst/src", Taken::Ask);
    assert_eq!(run(&mut op), Progress::Paused);
    assert_eq!(
        op.conflict(),
        Some(&Conflict {
            source: String::from("/src/one"),
            target: String::from("/dst/src/one"),
            occupant: Kind::File,
        })
    );
    op.resolve(Decision::Skip);
    assert_eq!(run(&mut op), Progress::Done);
    assert_eq!(tree.bytes("/dst/src/one").as_deref(), Some(&b"old"[..]));
    assert_eq!(tree.bytes("/dst/src/two").as_deref(), Some(&b"2"[..]));
    assert_eq!(op.report().skipped, 1);
    assert_eq!(
        op.report().done,
        1,
        "a directory merged into is no entry made"
    );
}

/// An overwrite removes the node the question was about and no other: one
/// put in its place meanwhile is asked about in turn.
#[test]
fn an_overwrite_removes_only_the_occupant_it_was_asked_about() {
    let tree = Tree::new()
        .file("/a", b"mine")
        .file("/b", b"theirs")
        .file("/other", b"other");
    let mut op = copying(&tree, "/a", "/b", Taken::Ask);
    assert_eq!(run(&mut op), Progress::Paused);
    tree.rename("/other", "/b");
    op.resolve(Decision::Overwrite);
    assert_eq!(run(&mut op), Progress::Paused, "asked again");
    assert_eq!(tree.bytes("/b").as_deref(), Some(&b"other"[..]));
    op.resolve(Decision::Overwrite);
    assert_eq!(run(&mut op), Progress::Done);
    assert_eq!(tree.bytes("/b").as_deref(), Some(&b"mine"[..]));
}

#[test]
fn a_link_at_the_target_is_replaced_and_never_written_through() {
    let tree = Tree::new()
        .file("/a", b"mine")
        .file("/victim", b"theirs")
        .link("/dst/to-victim", "/victim");
    let mut op = copying(&tree, "/a", "/dst/to-victim", Taken::Ask);
    assert_eq!(run(&mut op), Progress::Paused);
    assert_eq!(
        op.conflict().map(|conflict| conflict.occupant),
        Some(Kind::Link)
    );
    op.resolve(Decision::Overwrite);
    assert_eq!(run(&mut op), Progress::Done);
    assert_eq!(tree.kind("/dst/to-victim"), Some(Kind::File));
    assert_eq!(tree.bytes("/dst/to-victim").as_deref(), Some(&b"mine"[..]));
    assert_eq!(tree.bytes("/victim").as_deref(), Some(&b"theirs"[..]));
}

#[test]
fn overwriting_another_name_for_the_source_keeps_the_source_whole() {
    let tree = Tree::new()
        .file("/big", b"bytes")
        .hard_link("/twin", "/big");
    let mut op = copying(&tree, "/big", "/twin", Taken::Ask);
    assert_eq!(run(&mut op), Progress::Paused);
    op.resolve(Decision::Overwrite);
    assert_eq!(run(&mut op), Progress::Done);
    assert_eq!(tree.bytes("/big").as_deref(), Some(&b"bytes"[..]));
    assert_eq!(tree.bytes("/twin").as_deref(), Some(&b"bytes"[..]));
    assert_ne!(tree.id("/twin"), tree.id("/big"));
}

#[test]
fn a_copy_onto_its_sources_only_name_is_refused() {
    let tree = Tree::new().file("/d/f", b"f");
    let mut op = copying(&tree, "/d/f", "/d/f", Taken::Ask);
    assert_eq!(
        run(&mut op),
        Progress::Failed(OpError::Write(String::from("/d/f"), Errno::AlreadyExists))
    );
    assert_eq!(tree.bytes("/d/f").as_deref(), Some(&b"f"[..]));
}

#[test]
fn a_refusing_transfer_keeps_every_taken_name() {
    let tree = Tree::new()
        .file("/a", b"a")
        .file("/dst/a", b"there")
        .dir("/d/x")
        .dir("/dst/x");
    let mut op = copying(&tree, "/a", "/dst/a", Taken::Refuse);
    assert_eq!(
        run(&mut op),
        Progress::Failed(OpError::Write(String::from("/dst/a"), Errno::AlreadyExists))
    );
    let mut op = copying(&tree, "/d/x", "/dst/x", Taken::Refuse);
    assert_eq!(
        run(&mut op),
        Progress::Failed(OpError::Write(String::from("/dst/x"), Errno::AlreadyExists)),
        "never merged into"
    );
    let mut op = moving(&tree, "/a", "/dst/a", Taken::Refuse);
    assert_eq!(
        run(&mut op),
        Progress::Failed(OpError::Rename(String::from("/a"), Errno::AlreadyExists))
    );
    assert_eq!(tree.bytes("/dst/a").as_deref(), Some(&b"there"[..]));
    assert_eq!(tree.bytes("/a").as_deref(), Some(&b"a"[..]));
}

#[test]
fn a_directory_never_lands_where_a_leaf_is_nor_a_leaf_where_a_directory_is() {
    let tree = Tree::new()
        .file("/f", b"f")
        .dir("/d")
        .dir("/dst/f")
        .file("/dst/d", b"x")
        .link("/dst/l", "/d");
    let mismatch = |from: &str, to: &str| {
        let mut op = copying(&tree, from, to, Taken::Ask);
        run(&mut op)
    };
    assert_eq!(
        mismatch("/f", "/dst/f"),
        Progress::Failed(OpError::KindMismatch(String::from("/dst/f")))
    );
    assert_eq!(
        mismatch("/d", "/dst/d"),
        Progress::Failed(OpError::KindMismatch(String::from("/dst/d")))
    );
    assert_eq!(
        mismatch("/d", "/dst/l"),
        Progress::Failed(OpError::KindMismatch(String::from("/dst/l"))),
        "a link is a leaf however it resolves"
    );
}

#[test]
fn a_directory_is_never_copied_into_itself_however_it_is_spelled() {
    let tree = Tree::new()
        .file("/a/f", b"f")
        .dir("/b")
        .link("/b/to-a", "/a");
    let mut op = copying(&tree, "/a", "/a/inner", Taken::Ask);
    assert_eq!(run(&mut op), Progress::Failed(OpError::IntoItself));
    let mut op = copying(&tree, "/a", "/b/to-a/c", Taken::Ask);
    assert_eq!(run(&mut op), Progress::Failed(OpError::IntoItself));
    assert!(!tree.exists("/a/inner") && !tree.exists("/a/c"));
}

#[test]
fn a_directory_renamed_into_the_source_meanwhile_stops_the_copy_there() {
    let tree = Tree::new().file("/a/sub/f", b"f").dir("/b");
    tree.before_line(
        "entries /a/sub",
        Box::new(|tree: &Tree| tree.rename("/b", "/a/sub/b")),
    );
    let mut op = copying(&tree, "/a", "/b/c", Taken::Ask);
    assert_eq!(run(&mut op), Progress::Failed(OpError::IntoItself));
    assert!(!tree.exists("/a/sub/b/c/sub/b/c"));
}

#[test]
fn a_failure_part_way_removes_the_file_it_was_writing_and_settles_what_it_made() {
    let tree = Tree::new()
        .file("/ro/a", b"a")
        .file("/ro/f", b"f")
        .with_mode("/ro", 0o500)
        .refuse("read /ro/f", Errno::MediumError);
    let mut op = copying(&tree, "/ro", "/copy", Taken::Ask);
    assert_eq!(
        run(&mut op),
        Progress::Failed(OpError::Read(String::from("/ro/f"), Errno::MediumError))
    );
    assert!(!tree.exists("/copy/f"), "the partial file is removed");
    assert_eq!(tree.bytes("/copy/a").as_deref(), Some(&b"a"[..]));
    assert_eq!(tree.mode("/copy"), Some(0o500));
    assert_eq!(
        run(&mut op),
        Progress::Failed(OpError::Read(String::from("/ro/f"), Errno::MediumError)),
        "said again on every later advance"
    );
}

#[test]
fn an_attribute_refused_for_a_fault_fails_the_copy_and_removes_its_target() {
    let tree = Tree::new()
        .file("/src/f", b"f")
        .attr("/src/f", "mac.type", b"TEXT")
        .refuses_attr("mac.type", Errno::DeviceFault)
        .dir("/dst");
    let mut op = copying(&tree, "/src/f", "/dst/f", Taken::Ask);
    assert_eq!(
        run(&mut op),
        Progress::Failed(OpError::Attribute {
            path: String::from("/dst/f"),
            key: String::from("mac.type"),
            errno: Errno::DeviceFault,
        })
    );
    assert!(!tree.exists("/dst/f"));
}

#[test]
fn what_a_destination_cannot_keep_is_noted_and_the_copy_goes_on() {
    let tree = Tree::new()
        .file("/src/f", b"f")
        .attr("/src/f", "mac.type", b"TEXT")
        .refuses_attr("mac.type", Errno::NotSupported)
        .dir("/dst");
    let mut op = copying(&tree, "/src/f", "/dst/f", Taken::Ask);
    assert_eq!(run(&mut op), Progress::Done);
    let report = op.omissions().report("test").expect("something was left");
    assert!(report.summary.contains("mac.type"), "{}", report.summary);
    assert_eq!(tree.bytes("/dst/f").as_deref(), Some(&b"f"[..]));
}

#[test]
fn a_cancel_at_a_question_settles_what_was_made() {
    let tree = Tree::new()
        .file("/ro/a", b"a")
        .file("/ro/b", b"b")
        .with_mode("/ro", 0o500);
    tree.before_line(
        "create_file /copy/b",
        Box::new(|tree: &Tree| tree.write("/copy/b", b"theirs")),
    );
    let mut op = copying(&tree, "/ro", "/copy", Taken::Ask);
    assert_eq!(run(&mut op), Progress::Paused);
    op.resolve(Decision::Cancel);
    assert_eq!(op.conflict(), None);
    assert_eq!(run(&mut op), Progress::Done);
    assert!(op.report().cancelled);
    assert_eq!(tree.mode("/copy"), Some(0o500));
    assert_eq!(tree.bytes("/copy/b").as_deref(), Some(&b"theirs"[..]));
}

#[test]
fn a_cancel_mid_file_removes_the_file_it_was_writing() {
    let tree = Tree::new()
        .file("/big", &alloc::vec![7u8; 3 << 20])
        .dir("/dst");
    let mut op = copying(&tree, "/big", "/dst/big", Taken::Ask);
    let mut buf = Vec::new();
    // The look, the open, the make and a first chunk.
    for _ in 0..4 {
        assert_eq!(op.advance(1, &mut buf), Progress::Yielded);
    }
    assert!(tree.exists("/dst/big"));
    op.resolve(Decision::Cancel);
    assert_eq!(run(&mut op), Progress::Done);
    assert!(op.report().cancelled);
    assert!(!tree.exists("/dst/big"), "the half-written file is removed");
}

#[test]
fn a_moves_copy_is_put_on_the_medium_before_its_source_goes_and_a_copy_never_waits_on_it() {
    let tree = Tree::new().file("/src/inner/f", b"f").volume("/dst");
    let mut op = moving(&tree, "/src", "/dst/src", Taken::Ask);
    assert_eq!(run(&mut op), Progress::Done);
    assert!(index_of(&tree, "sync /dst/src") < index_of(&tree, "unlink /src/inner/f"));
    let tree = Tree::new().file("/src/inner/f", b"f").volume("/dst");
    let mut op = copying(&tree, "/src", "/dst/src", Taken::Ask);
    assert_eq!(run(&mut op), Progress::Done);
    assert!(
        tree.calls().iter().all(|call| !call.starts_with("sync ")),
        "{:?}",
        tree.calls()
    );
}

#[test]
fn a_move_on_one_volume_is_one_rename() {
    let tree = Tree::new().file("/a/f", b"f").dir("/b");
    let mut op = moving(&tree, "/a", "/b/a", Taken::Ask);
    assert_eq!(run(&mut op), Progress::Done);
    assert!(!tree.exists("/a"));
    assert_eq!(tree.bytes("/b/a/f").as_deref(), Some(&b"f"[..]));
    assert_eq!(op.report().done, 1);
    let calls = tree.calls();
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.starts_with("rename"))
            .count(),
        1
    );
    assert!(calls
        .iter()
        .all(|call| !call.starts_with("create_file") && !call.starts_with("make_dir")));
}

#[test]
fn a_move_across_volumes_copies_keeps_its_modes_and_then_removes_what_it_copied() {
    let tree = Tree::new()
        .file("/src/f", b"f")
        .link("/src/l", "f")
        .dir("/src/sub")
        .with_mode("/src/f", 0o640)
        .with_mode("/src/sub", 0o750)
        .dir("/vol")
        .volume("/vol")
        .with_umask(0o077);
    let mut op = moving(&tree, "/src", "/vol/src", Taken::Ask);
    assert_eq!(run(&mut op), Progress::Done);
    assert!(!tree.exists("/src"));
    assert_eq!(tree.bytes("/vol/src/f").as_deref(), Some(&b"f"[..]));
    assert_eq!(
        tree.mode("/vol/src/f"),
        Some(0o640),
        "its own, not less the umask"
    );
    assert_eq!(tree.mode("/vol/src/sub"), Some(0o750));
    assert_eq!(tree.link_target("/vol/src/l").as_deref(), Some("f"));
    assert_eq!(op.report().done, 4);
    assert!(op.report().kept.is_empty());
    let synced = index_of(&tree, "sync /vol/src");
    let first_removal = tree
        .calls()
        .iter()
        .position(|call| call.starts_with("unlink /src"))
        .expect("the source was removed");
    assert!(synced < first_removal, "on the medium before anything goes");
}

#[test]
fn a_moved_file_keeps_its_owner_where_it_may_and_never_a_set_id_bit_held_for_another() {
    let theirs = || {
        Tree::new()
            .file("/src/tool", b"t")
            .with_mode("/src/tool", 0o4755)
            .owned("/src/tool", 2000, 2000)
            .dir("/vol")
            .volume("/vol")
    };
    let tree = theirs();
    let mut op = moving(&tree, "/src/tool", "/vol/tool", Taken::Ask);
    assert_eq!(run(&mut op), Progress::Done);
    assert_eq!(
        (tree.owner("/vol/tool"), tree.mode("/vol/tool")),
        (Some(USER), Some(0o755)),
        "kept by the mover, without the set-ID bit"
    );
    let tree = theirs().privileged();
    let mut op = moving(&tree, "/src/tool", "/vol/tool", Taken::Ask);
    assert_eq!(run(&mut op), Progress::Done);
    assert_eq!(
        (tree.owner("/vol/tool"), tree.mode("/vol/tool")),
        (Some((2000, 2000)), Some(0o755)),
        "given back, its bits set first; the owner's change clears the set-ID bit"
    );
}

#[test]
fn a_move_across_volumes_keeps_an_entry_changed_or_added_while_it_copied() {
    let tree = Tree::new()
        .file("/src/kept", b"before")
        .file("/src/other", b"o")
        .dir("/vol")
        .volume("/vol");
    tree.before_line(
        "sync /vol/src",
        Box::new(|tree: &Tree| {
            tree.write("/src/kept", b"before, and after");
            tree.write("/src/added", b"new");
        }),
    );
    let mut op = moving(&tree, "/src", "/vol/src", Taken::Ask);
    assert_eq!(run(&mut op), Progress::Done);
    assert_eq!(op.report().kept, ["/src/kept", "/src/added"]);
    assert_eq!(
        tree.bytes("/src/kept").as_deref(),
        Some(&b"before, and after"[..])
    );
    assert!(tree.exists("/src/added") && !tree.exists("/src/other"));
    assert_eq!(tree.kind("/src"), Some(Kind::Directory));
    assert_eq!(tree.bytes("/vol/src/kept").as_deref(), Some(&b"before"[..]));
}

#[test]
fn a_skip_in_a_move_across_volumes_keeps_the_skipped_source() {
    let tree = Tree::new()
        .file("/src/data", b"7")
        .file("/vol/data", b"9")
        .volume("/vol");
    let mut op = moving(&tree, "/src/data", "/vol/data", Taken::Ask);
    assert_eq!(run(&mut op), Progress::Paused);
    op.resolve(Decision::Skip);
    assert_eq!(run(&mut op), Progress::Done);
    assert_eq!(tree.bytes("/vol/data").as_deref(), Some(&b"9"[..]));
    assert_eq!(tree.bytes("/src/data").as_deref(), Some(&b"7"[..]));
    assert_eq!(op.report().skipped, 1);
    assert!(op.report().kept.is_empty(), "a skip is no change to report");
}

#[test]
fn a_copy_goes_on_into_a_volume_attached_inside_its_source() {
    let tree = Tree::new()
        .file("/src/mnt/f", b"f")
        .mount_point("/src/mnt")
        .dir("/dst");
    let mut op = copying(&tree, "/src", "/dst/src", Taken::Ask);
    assert_eq!(run(&mut op), Progress::Done);
    assert_eq!(tree.bytes("/dst/src/mnt/f").as_deref(), Some(&b"f"[..]));
}

#[test]
fn a_move_replaces_a_directory_only_while_it_is_empty_on_any_volume() {
    let tree = Tree::new()
        .file("/src/data", b"7")
        .with_mode("/src", 0o750)
        .dir("/vol/src")
        .with_mode("/vol/src", 0o700)
        .volume("/vol");
    let mut op = moving(&tree, "/src", "/vol/src", Taken::Ask);
    assert_eq!(run(&mut op), Progress::Done);
    assert_eq!(tree.bytes("/vol/src/data").as_deref(), Some(&b"7"[..]));
    assert_eq!(
        tree.mode("/vol/src"),
        Some(0o750),
        "the source's, not the empty one's"
    );
    assert!(!tree.exists("/src"));
    let tree = Tree::new()
        .file("/src/data", b"7")
        .file("/vol/src/kept", b"k")
        .volume("/vol");
    let mut op = moving(&tree, "/src", "/vol/src", Taken::Ask);
    assert_eq!(
        run(&mut op),
        Progress::Failed(OpError::Rename(String::from("/vol/src"), Errno::NotEmpty))
    );
    assert_eq!(tree.bytes("/src/data").as_deref(), Some(&b"7"[..]));
    assert_eq!(tree.bytes("/vol/src/kept").as_deref(), Some(&b"k"[..]));
    assert!(!tree.exists("/vol/src/data"));
}

#[test]
fn an_overwrite_keeps_what_it_replaces_until_the_replacement_is_whole() {
    // A failing medium mid-copy: the occupant is still there.
    let tree = Tree::new()
        .file("/a", b"new bytes")
        .file("/b", b"old bytes")
        .refuse("write /b.staged", Errno::DeviceFault);
    let mut op = copying(&tree, "/a", "/b", Taken::Ask);
    assert_eq!(run(&mut op), Progress::Paused);
    op.resolve(Decision::Overwrite);
    assert!(matches!(
        run(&mut op),
        Progress::Failed(OpError::Write(_, Errno::DeviceFault))
    ));
    assert_eq!(tree.bytes("/b").as_deref(), Some(&b"old bytes"[..]));
    assert!(
        !tree.exists("/b.staged"),
        "the part-built replacement is removed"
    );
    // A move on one volume whose source went while it asked.
    let tree = Tree::new().file("/a", b"a").file("/b", b"old");
    let mut op = moving(&tree, "/a", "/b", Taken::Ask);
    assert_eq!(run(&mut op), Progress::Paused);
    tree.remove("/a");
    op.resolve(Decision::Overwrite);
    assert!(matches!(run(&mut op), Progress::Failed(_)));
    assert_eq!(tree.bytes("/b").as_deref(), Some(&b"old"[..]));
    // A whole replacement goes over the name in one rename, after it is on
    // the medium.
    let tree = Tree::new().file("/a", b"new").file("/b", b"old");
    let mut op = copying(&tree, "/a", "/b", Taken::Ask);
    assert_eq!(run(&mut op), Progress::Paused);
    op.resolve(Decision::Overwrite);
    assert_eq!(run(&mut op), Progress::Done);
    assert_eq!(tree.bytes("/b").as_deref(), Some(&b"new"[..]));
    assert!(!tree.exists("/b.staged"));
    assert!(index_of(&tree, "sync /b.staged") < index_of(&tree, "rename /b.staged -> /b"));
    // No staging name, no overwrite: the occupant stays.
    let tree = Tree::new().file("/a", b"new").file("/locked", b"old");
    let mut op = copying(&tree, "/a", "/locked", Taken::Ask);
    assert_eq!(run(&mut op), Progress::Paused);
    op.resolve(Decision::Overwrite);
    assert_eq!(
        run(&mut op),
        Progress::Failed(OpError::Write(String::from("/locked"), Errno::Busy))
    );
    assert_eq!(tree.bytes("/locked").as_deref(), Some(&b"old"[..]));
}

#[test]
fn a_move_whose_source_changed_since_its_look_is_refused() {
    let tree = Tree::new().file("/a", b"a").file("/swap", b"s").dir("/b");
    tree.before_line(
        "rename /a -> /b/a",
        Box::new(|tree: &Tree| tree.rename("/swap", "/a")),
    );
    let mut op = moving(&tree, "/a", "/b/a", Taken::Ask);
    assert_eq!(
        run(&mut op),
        Progress::Failed(OpError::Changed(String::from("/a")))
    );
    assert_eq!(tree.bytes("/a").as_deref(), Some(&b"s"[..]));
    assert!(!tree.exists("/b/a"));
}

/// An operation begun on a source.
type Start = fn(Named<Handle>, &Tree) -> Operation<Handle>;

#[test]
fn a_source_no_longer_the_node_its_listing_showed_is_left_alone() {
    let ops: [Start; 3] = [
        |source, tree| {
            Operation::transfer(Verb::Copy, source, named(tree, "/b/a"), Taken::Ask, staged)
        },
        |source, tree| {
            Operation::transfer(Verb::Move, source, named(tree, "/b/a"), Taken::Ask, staged)
        },
        |source, _| Operation::remove(source),
    ];
    for op in ops {
        let tree = Tree::new()
            .file("/a", b"seen")
            .file("/swap", b"s")
            .dir("/b");
        let seen = tree.id("/a").expect("listed");
        tree.rename("/swap", "/a");
        let mut swapped = op(
            Named {
                seen,
                ..named(&tree, "/a")
            },
            &tree,
        );
        assert_eq!(
            run(&mut swapped),
            Progress::Failed(OpError::Changed(String::from("/a")))
        );
        assert_eq!(tree.bytes("/a").as_deref(), Some(&b"s"[..]));
        assert!(!tree.exists("/b/a"));

        let seen = tree.id("/a").expect("listed");
        let mut current = op(
            Named {
                seen,
                ..named(&tree, "/a")
            },
            &tree,
        );
        assert_eq!(run(&mut current), Progress::Done);
    }
}

#[test]
fn a_move_asks_about_a_name_taken_after_its_look() {
    let tree = Tree::new().file("/a", b"mine").dir("/b");
    tree.before_line(
        "rename /a -> /b/a",
        Box::new(|tree: &Tree| tree.write("/b/a", b"theirs")),
    );
    let mut op = moving(&tree, "/a", "/b/a", Taken::Ask);
    assert_eq!(run(&mut op), Progress::Paused);
    assert_eq!(
        op.conflict().map(|conflict| conflict.target.as_str()),
        Some("/b/a")
    );
    op.resolve(Decision::Skip);
    assert_eq!(run(&mut op), Progress::Done);
    assert_eq!(tree.bytes("/a").as_deref(), Some(&b"mine"[..]));
    assert_eq!(tree.bytes("/b/a").as_deref(), Some(&b"theirs"[..]));
}

#[test]
fn a_removal_takes_a_tree_entries_first_and_a_link_as_itself() {
    let tree = Tree::new()
        .file("/d/f", b"f")
        .file("/d/sub/g", b"g")
        .link("/d/to-victim", "/victim")
        .file("/victim/precious", b"theirs");
    let mut op = removing(&tree, "/d");
    assert_eq!(run(&mut op), Progress::Done);
    assert!(!tree.exists("/d"));
    assert_eq!(
        tree.bytes("/victim/precious").as_deref(),
        Some(&b"theirs"[..])
    );
    assert_eq!(op.report().done, 5);
    assert!(index_of(&tree, "unlink /d/sub/g") < index_of(&tree, "unlink /d/sub"));
    assert!(index_of(&tree, "unlink /d/sub") < index_of(&tree, "unlink /d"));
}

#[test]
fn a_removal_keeps_and_names_an_entry_put_in_place_meanwhile() {
    let tree = Tree::new().file("/d/f", b"f").file("/swap", b"s");
    tree.before_line(
        "unlink /d/f",
        Box::new(|tree: &Tree| tree.rename("/swap", "/d/f")),
    );
    let mut op = removing(&tree, "/d");
    assert_eq!(run(&mut op), Progress::Done);
    assert_eq!(op.report().kept, ["/d/f"]);
    assert_eq!(tree.bytes("/d/f").as_deref(), Some(&b"s"[..]));
    assert_eq!(tree.kind("/d"), Some(Kind::Directory));
}

#[test]
fn a_failed_look_names_the_source() {
    let tree = Tree::new().dir("/dst");
    let mut op = copying(&tree, "/missing", "/dst/x", Taken::Ask);
    assert_eq!(
        run(&mut op),
        Progress::Failed(OpError::Read(String::from("/missing"), Errno::NotFound))
    );
}

#[test]
fn a_move_across_volumes_says_while_it_removes_what_it_copied() {
    let tree = Tree::new()
        .file("/src/a", b"a")
        .file("/src/b", b"b")
        .dir("/vol")
        .volume("/vol");
    let mut buf = Vec::new();
    let mut op = moving(&tree, "/src", "/vol/src", Taken::Ask);
    let mut seen = Vec::new();
    while op.advance(1, &mut buf) == Progress::Yielded {
        seen.push((op.removing(), op.report().done));
    }
    assert!(!tree.exists("/src"));
    let first = seen
        .iter()
        .position(|&(removing, _)| removing)
        .unwrap_or_else(|| panic!("it never said it was removing: {seen:?}"));
    assert!(
        seen[first..]
            .iter()
            .all(|&(removing, done)| removing && done == 3),
        "removals count nothing and nothing follows them: {seen:?}"
    );
    assert!(!op.removing(), "it has ended");
    let mut op = removing(&tree, "/vol/src");
    while op.advance(1, &mut buf) == Progress::Yielded {
        assert!(!op.removing(), "a removal of its own is no move's");
    }
    assert_eq!(op.report().done, 3);
}

#[test]
fn a_refusal_names_its_entry_unless_it_spells_none() {
    let read = OpError::Read(String::from("/a"), Errno::NotFound);
    assert_eq!(read.path(), Some("/a"));
    let attribute = OpError::Attribute {
        path: String::from("/b"),
        key: String::from("user.k"),
        errno: Errno::DeviceFault,
    };
    assert_eq!(attribute.path(), Some("/b"));
    assert_eq!(
        OpError::Read(String::new(), Errno::OutOfMemory).path(),
        None
    );
    assert_eq!(OpError::IntoItself.path(), None);
}

/// A run sliced a unit at a time does exactly what one run does.
#[test]
fn a_slice_of_one_unit_resumed_to_the_end_equals_one_unbounded_advance() {
    let build = || {
        Tree::new()
            .file("/src/big", &alloc::vec![1u8; (5 << 20) / 2])
            .file("/src/sub/x", b"x")
            .link("/src/l", "big")
            .dir("/vol")
            .volume("/vol")
    };
    for verb in [Verb::Copy, Verb::Move] {
        let act = |tree: &Tree| {
            Operation::transfer(
                verb,
                named(tree, "/src"),
                named(tree, "/vol/src"),
                Taken::Ask,
                staged,
            )
        };
        let whole = build();
        let mut op = act(&whole);
        assert_eq!(run(&mut op), Progress::Done);
        let report = op.report().clone();
        let sliced = build();
        let mut op = act(&sliced);
        let mut buf = Vec::new();
        let mut yields = 0;
        loop {
            match op.advance(1, &mut buf) {
                Progress::Yielded => yields += 1,
                progress => {
                    assert_eq!(progress, Progress::Done);
                    break;
                }
            }
        }
        assert!(yields > 8, "{verb:?}: {yields} yields");
        assert_eq!(sliced.paths(), whole.paths(), "{verb:?}");
        assert_eq!(sliced.calls(), whole.calls(), "{verb:?}");
        assert_eq!(op.report(), &report, "{verb:?}");
    }
}

/// A source holding a directory to swap for a link to a victim beside it.
fn bait() -> Tree {
    Tree::new()
        .file("/src/f", b"f")
        .file("/src/sub/g", b"g")
        .file("/victim/precious", b"theirs")
        .link("/bait", "/victim")
        .dir("/dst")
}

/// Swap `/src/sub`, while it is still the directory, for the link to the
/// victim.
fn swap(tree: &Tree) {
    if tree.kind("/src/sub") == Some(Kind::Directory) {
        tree.rename("/src/sub", "/aside");
        tree.rename("/bait", "/src/sub");
    }
}

/// A merging copy's target already holding the directory a source's lands
/// in, and a name in it the copy asks about, to swap for a link to a victim.
fn target_bait() -> Tree {
    Tree::new()
        .file("/src/sub/g", b"g")
        .file("/src/sub/h", b"h")
        .file("/dst/src/sub/g", b"old")
        .file("/victim/precious", b"theirs")
        .link("/bait", "/victim")
}

/// Swap the target's `/dst/src/sub`, while it is still the directory, for
/// the link to the victim.
fn swap_target(tree: &Tree) {
    if tree.kind("/dst/src/sub") == Some(Kind::Directory) {
        tree.rename("/dst/src/sub", "/aside");
        tree.rename("/bait", "/dst/src/sub");
    }
}

/// How a sweep builds its tree, and swaps a directory in it for a link.
type Bait = (fn() -> Tree, fn(&Tree));

/// How a sweep begins its operation, and answers its questions.
type Act = (
    fn(&Tree) -> Operation<Handle>,
    fn(&mut Operation<Handle>) -> Progress,
);

/// Run `act` over `build`'s tree with the swap before each of its calls in
/// turn, skipping what it asks about: the victim is never read through,
/// written through, changed or removed.
fn sweep(build: fn() -> Tree, act: fn(&Tree) -> Operation<Handle>, what: &str) {
    sweep_with((build, swap), (act, run_out), what);
}

/// [`sweep`] over any bait, answering as `drive` does.
fn sweep_with((build, swap): Bait, (act, drive): Act, what: &str) {
    let calm = build();
    drive(&mut act(&calm));
    for n in 0..calm.calls().len() {
        let tree = build();
        tree.before_call(n, Box::new(swap));
        drive(&mut act(&tree));
        assert_eq!(
            tree.bytes("/victim/precious").as_deref(),
            Some(&b"theirs"[..]),
            "{what}: swapped before call {n}"
        );
        assert_eq!(
            (tree.mode("/victim"), tree.mode("/victim/precious")),
            (Some(0o755), Some(0o644)),
            "{what}: swapped before call {n}"
        );
        let victim: Vec<String> = tree
            .paths()
            .into_iter()
            .filter(|path| path.starts_with("/victim"))
            .collect();
        assert_eq!(victim, ["/victim", "/victim/precious"], "{what}: {n}");
        assert!(
            tree.calls().iter().all(|call| !call.contains("/victim")),
            "{what}: reached through the link swapped before call {n}: {:?}",
            tree.calls()
        );
    }
}

#[test]
fn a_directory_swapped_for_a_link_at_any_point_never_leads_a_copy_to_its_target() {
    sweep(
        bait,
        |tree| copying(tree, "/src", "/dst/src", Taken::Ask),
        "copy",
    );
}

#[test]
fn a_directory_swapped_for_a_link_at_any_point_never_leads_a_move_to_its_target() {
    sweep(
        bait,
        |tree| moving(tree, "/src", "/dst/src", Taken::Ask),
        "move",
    );
    sweep(
        || bait().volume("/dst"),
        |tree| moving(tree, "/src", "/dst/src", Taken::Ask),
        "move across volumes",
    );
}

#[test]
fn a_directory_swapped_for_a_link_at_any_point_never_leads_a_removal_to_its_target() {
    sweep(bait, |tree| removing(tree, "/src"), "removal");
}

#[test]
fn a_target_directory_swapped_for_a_link_at_any_point_never_leads_a_merge_to_its_target() {
    let merge: fn(&Tree) -> Operation<Handle> =
        |tree| copying(tree, "/src", "/dst/src", Taken::Ask);
    sweep_with(
        (target_bait, swap_target),
        (merge, run_out),
        "merge, skipping",
    );
    sweep_with(
        (target_bait, swap_target),
        (merge, run_over),
        "merge, overwriting",
    );
}

#[test]
fn a_move_across_volumes_under_refuse_carries_a_free_name_and_keeps_a_taken_one() {
    let tree = Tree::new()
        .file("/src/d/f", b"f")
        .file("/src/kept", b"mine")
        .file("/vol/kept", b"theirs")
        .volume("/vol");
    let mut op = moving(&tree, "/src/d", "/vol/d", Taken::Refuse);
    assert_eq!(run(&mut op), Progress::Done);
    assert_eq!(tree.bytes("/vol/d/f").as_deref(), Some(&b"f"[..]));
    assert!(!tree.exists("/src/d"));

    let mut op = moving(&tree, "/src/kept", "/vol/kept", Taken::Refuse);
    assert!(matches!(run(&mut op), Progress::Failed(_)));
    assert_eq!(tree.bytes("/src/kept").as_deref(), Some(&b"mine"[..]));
    assert_eq!(tree.bytes("/vol/kept").as_deref(), Some(&b"theirs"[..]));
}

#[test]
fn a_refusal_while_a_move_removes_its_source_keeps_the_rest_and_the_whole_copy() {
    let tree = Tree::new()
        .file("/src/a", b"a")
        .file("/src/b", b"b")
        .dir("/vol")
        .volume("/vol")
        .refuse("unlink /src/b", Errno::PermissionDenied);
    let mut op = moving(&tree, "/src", "/vol/src", Taken::Ask);
    assert_eq!(
        run(&mut op),
        Progress::Failed(OpError::Remove(
            String::from("/src/b"),
            Errno::PermissionDenied
        ))
    );
    assert_eq!(tree.bytes("/vol/src/a").as_deref(), Some(&b"a"[..]));
    assert_eq!(tree.bytes("/vol/src/b").as_deref(), Some(&b"b"[..]));
    assert_eq!(tree.bytes("/src/b").as_deref(), Some(&b"b"[..]));
}

#[test]
fn a_cancel_while_a_move_removes_its_source_stops_there_with_the_copy_whole() {
    let tree = Tree::new()
        .file("/src/a", b"a")
        .file("/src/b", b"b")
        .dir("/vol")
        .volume("/vol");
    let mut op = moving(&tree, "/src", "/vol/src", Taken::Ask);
    let mut buf = Vec::new();
    while !op.removing() {
        assert_eq!(op.advance(1, &mut buf), Progress::Yielded);
    }
    op.resolve(Decision::Cancel);
    assert_eq!(run(&mut op), Progress::Done);
    assert!(op.report().cancelled);
    assert_eq!(tree.bytes("/vol/src/a").as_deref(), Some(&b"a"[..]));
    assert_eq!(tree.bytes("/vol/src/b").as_deref(), Some(&b"b"[..]));
    assert!(tree.exists("/src/a") && tree.exists("/src/b"));
}
