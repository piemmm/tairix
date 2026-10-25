//! `link(2)` conformance: a second directory entry for one inode, and the
//! lifecycle that makes it safe — a name goes, the storage stays until the
//! last one does (`plans/SYMLINKS.md` S6).
//!
//! Driven against a **real** ARXFS volume through the real filesystem
//! service, so the on-disk `nlink` accounting, the per-component resolution,
//! and the follow posture of each operand are all the production code paths.

use tairix_test_posix_fs_suite::*;

/// Add `link` as a second name for `existing` under the volume root, as the
/// volume's owner, keeping the existing name's final component as typed.
fn make_link(vol: &Volume, existing: &str, link: &str) -> Result<(), Errno> {
    vol.link(
        ROOT.caller(),
        &vol_path(existing),
        &vol_path(link),
        FinalLink::Keep,
    )
}

/// A file at `name` holding `body`.
fn make_file(vol: &Volume, name: &str, body: &[u8]) {
    let owner = ROOT.caller();
    vol.create(owner, &vol_path(name), new_file())
        .unwrap_or_else(|e| panic!("create {name}: {e:?}"));
    if !body.is_empty() {
        vol.write(owner, &vol_path(name), 0, body)
            .unwrap_or_else(|e| panic!("write {name}: {e:?}"));
    }
}

/// What the name `name` itself is, a final link kept.
fn lstat(vol: &Volume, name: &str) -> Result<FileStat, Errno> {
    vol.stat(ROOT.caller(), &vol_path(name), FinalLink::Keep)
}

#[test]
fn two_names_reach_one_inode_and_one_stat() {
    // Not two files with equal contents but one file with two names.
    let vol = arxfs_volume(false);

    make_file(&vol, "first", b"shared");
    make_link(&vol, "first", "second").expect("add a second name");

    let first = lstat(&vol, "first").expect("stat the first name");
    let second = lstat(&vol, "second").expect("stat the second name");
    assert_eq!(first.id, second.id, "one node, two names");
    assert_eq!(first.nlink, 2);
    assert_eq!(second.nlink, 2);
    assert_eq!(first.size, second.size);
}

#[test]
fn a_write_through_one_name_is_visible_through_the_other() {
    let vol = arxfs_volume(false);
    let owner = ROOT.caller();

    make_file(&vol, "first", b"before");
    make_link(&vol, "first", "second").expect("add a second name");

    vol.write(owner, &vol_path("second"), 0, b"after ")
        .expect("write through the second name");
    let mut buf = [0u8; 6];
    let read = vol
        .read(owner, &vol_path("first"), 0, &mut buf)
        .expect("read through the first name");
    assert_eq!(&buf[..read], b"after ");
}

#[test]
fn unlinking_one_name_leaves_the_other_readable() {
    // An unlink that is not the last frees nothing, or it destroys data the
    // remaining name reaches.
    let vol = arxfs_volume(false);
    let owner = ROOT.caller();

    make_file(&vol, "first", b"payload");
    make_link(&vol, "first", "second").expect("add a second name");

    vol.unlink(owner, &vol_path("first"), UnlinkFlags::empty())
        .expect("drop the first name");
    assert_eq!(lstat(&vol, "first").map(|i| i.size), Err(Errno::NotFound));
    let survivor = lstat(&vol, "second").expect("the other name still resolves");
    assert_eq!(survivor.nlink, 1, "one name went, one remains");

    let mut buf = [0u8; 7];
    let read = vol
        .read(owner, &vol_path("second"), 0, &mut buf)
        .expect("the bytes are still there");
    assert_eq!(&buf[..read], b"payload");
}

#[test]
fn unlinking_the_last_name_frees_the_storage() {
    let vol = arxfs_volume(false);
    let owner = ROOT.caller();
    let free = || vol.driver().stats().expect("stats").free_blocks;

    let empty = free();
    make_file(&vol, "first", b"payload");
    make_link(&vol, "first", "second").expect("add a second name");
    let with_data = free();
    assert!(with_data < empty, "the file occupies blocks");

    vol.unlink(owner, &vol_path("first"), UnlinkFlags::empty())
        .expect("drop one name");
    assert_eq!(free(), with_data, "a name went, no storage did");

    vol.unlink(owner, &vol_path("second"), UnlinkFlags::empty())
        .expect("drop the last name");
    assert!(free() > with_data, "the last name returns the blocks");
}

#[test]
fn the_count_changes_with_each_name() {
    let vol = arxfs_volume(false);
    let count = |name: &str| lstat(&vol, name).expect("stat").nlink;

    make_file(&vol, "one", b"x");
    assert_eq!(count("one"), 1);
    make_link(&vol, "one", "two").expect("second name");
    assert_eq!(count("one"), 2);
    make_link(&vol, "two", "three").expect("third name");
    assert_eq!(count("one"), 3);
    vol.unlink(ROOT.caller(), &vol_path("two"), UnlinkFlags::empty())
        .expect("drop one");
    assert_eq!(count("one"), 2);
}

#[test]
fn a_directory_is_refused_and_nothing_is_created() {
    // The tree must stay a tree: that is what makes the resolver's physical
    // `..` name the directory the walk actually came through.
    let vol = arxfs_volume(false);

    vol.mkdir(ROOT.caller(), &vol_path("dir"), new_dir())
        .expect("create the directory");
    assert_eq!(make_link(&vol, "dir", "alias"), Err(Errno::IsADirectory));
    assert_eq!(
        lstat(&vol, "alias").map(|i| i.size),
        Err(Errno::NotFound),
        "the refused name was never created"
    );
}

#[test]
fn a_new_name_off_this_volume_has_no_backing_to_land_in() {
    // A path under the in-RAM layout has no driver behind it, so there is
    // nowhere to write the entry. The refusal between two backed mounts is
    // exercised in `kernel/core`'s delegation suite.
    let vol = arxfs_volume(false);

    make_file(&vol, "here", b"x");
    assert_eq!(
        vol.link(
            ROOT.caller(),
            &vol_path("here"),
            "/Users/elsewhere",
            FinalLink::Keep
        ),
        Err(Errno::NotFound)
    );
}

#[test]
fn a_taken_name_is_refused_and_replaces_nothing() {
    let vol = arxfs_volume(false);

    make_file(&vol, "first", b"first body");
    make_file(&vol, "taken", b"occupant");
    assert_eq!(make_link(&vol, "first", "taken"), Err(Errno::AlreadyExists));

    let mut buf = [0u8; 8];
    let read = vol
        .read(ROOT.caller(), &vol_path("taken"), 0, &mut buf)
        .expect("read the occupant");
    assert_eq!(&buf[..read], b"occupant");
    assert_eq!(
        lstat(&vol, "first").expect("stat").nlink,
        1,
        "a refused link raises no count"
    );
}

#[test]
fn the_default_posture_names_a_symbolic_link_itself() {
    // POSIX `link()`: the node that gains a name is the one the caller
    // spelled, so a symbolic link planted on the way cannot redirect the new
    // name onto an object the caller never asked for.
    let vol = arxfs_volume(false);
    let owner = ROOT.caller();

    make_file(&vol, "target", b"target body");
    vol.symlink(owner, &vol_path("sym"), "target")
        .expect("create the symbolic link");
    make_link(&vol, "sym", "sym2").expect("a second name for the link");

    let second = lstat(&vol, "sym2").expect("lstat the second name");
    assert_eq!(second.kind, FileKind::Symlink, "the link itself gained it");
    assert_eq!(second.nlink, 2);
    assert_eq!(
        vol.readlink(owner, &vol_path("sym2")),
        Ok(String::from("target")),
        "both names read the same stored target"
    );
    assert_eq!(lstat(&vol, "target").expect("stat").nlink, 1);
}

#[test]
fn the_follow_posture_names_what_a_symbolic_link_names() {
    // `linkat(AT_SYMLINK_FOLLOW)`, which `ln -L` asks for: the second name
    // goes to the target, and the link is left alone.
    let vol = arxfs_volume(false);
    let owner = ROOT.caller();

    make_file(&vol, "target", b"target body");
    vol.symlink(owner, &vol_path("sym"), "target")
        .expect("create the symbolic link");
    vol.link(
        owner,
        &vol_path("sym"),
        &vol_path("hard"),
        FinalLink::Follow,
    )
    .expect("a second name for the target");

    let hard = lstat(&vol, "hard").expect("lstat the new name");
    assert_eq!(hard.kind, FileKind::Regular, "the target gained it");
    assert_eq!(hard.nlink, 2);
    let target = lstat(&vol, "target").expect("stat the target");
    assert_eq!(hard.id, target.id);
    let sym = lstat(&vol, "sym").expect("lstat the link");
    assert_eq!(sym.kind, FileKind::Symlink);
    assert_eq!(sym.nlink, 1);
}

#[test]
fn a_dangling_link_has_nothing_to_follow_to() {
    let vol = arxfs_volume(false);
    let owner = ROOT.caller();

    vol.symlink(owner, &vol_path("dangling"), "absent")
        .expect("create the dangling link");
    assert_eq!(
        vol.link(
            owner,
            &vol_path("dangling"),
            &vol_path("hard"),
            FinalLink::Follow,
        ),
        Err(Errno::NotFound)
    );
    // Kept, the link itself is a perfectly good node to name twice.
    make_link(&vol, "dangling", "alias").expect("name the link itself");
    assert_eq!(
        vol.readlink(owner, &vol_path("alias")),
        Ok(String::from("absent"))
    );
}

#[test]
fn a_missing_existing_name_creates_nothing() {
    let vol = arxfs_volume(false);

    assert_eq!(make_link(&vol, "absent", "alias"), Err(Errno::NotFound));
    assert_eq!(lstat(&vol, "alias").map(|i| i.size), Err(Errno::NotFound));
}

#[test]
fn a_read_only_mount_refuses_and_an_unauthorised_parent_refuses() {
    // The mount's own flag, then the caller's write permission on the new
    // name's parent.
    let read_only = arxfs_volume(true);
    assert_eq!(
        make_link(&read_only, "anything", "alias"),
        Err(Errno::ReadOnly)
    );

    let vol = arxfs_volume(false);
    make_file(&vol, "first", b"x");
    assert_eq!(
        vol.link(
            STRANGER.caller(),
            &vol_path("first"),
            &vol_path("alias"),
            FinalLink::Keep,
        ),
        Err(Errno::PermissionDenied)
    );
    assert_eq!(
        lstat(&vol, "alias").map(|i| i.size),
        Err(Errno::NotFound),
        "a refused link leaves no name behind"
    );
    make_link(&vol, "first", "alias").expect("the parent's owner may");
}

#[test]
fn a_second_name_survives_a_remount_with_its_count() {
    // The count is on-disk state, not a mount-time derivation.
    let vol = arxfs_volume(false);

    make_file(&vol, "first", b"payload");
    make_link(&vol, "first", "second").expect("add a second name");

    vol.remount();
    assert_eq!(
        lstat(&vol, "second").expect("stat after the remount").nlink,
        2
    );
    let mut buf = [0u8; 7];
    let read = vol
        .read(ROOT.caller(), &vol_path("second"), 0, &mut buf)
        .expect("read after the remount");
    assert_eq!(&buf[..read], b"payload");
}
