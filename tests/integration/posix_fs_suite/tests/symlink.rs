//! `symlink(2)` / `readlink(2)` conformance, and the follow matrix every
//! other operation takes around a link (`docs/src/filesystem/overview.md`
//! §"Which operations follow a final link").
//!
//! Driven against a **real** ARXFS volume through the real filesystem
//! service, so the on-disk spelling, the per-component resolution, and the
//! follow posture of each operation are all the production code paths.

use tairix_test_posix_fs_suite::*;

/// Create the link `link` naming `target` under the volume root, owned by the
/// volume's owner.
fn make_link(vol: &Volume, link: &str, target: &str) {
    vol.symlink(ROOT.caller(), &vol_path(link), target)
        .unwrap_or_else(|e| panic!("create the link {link} -> {target}: {e:?}"));
}

/// The kind of what `name` reaches, its final link treated as `final_link`.
fn kind_of(vol: &Volume, name: &str, final_link: FinalLink) -> Result<FileKind, Errno> {
    vol.stat(ROOT.caller(), &vol_path(name), final_link)
        .map(|info| info.kind)
}

#[test]
fn a_created_link_reads_its_target_back_verbatim() {
    let vol = arxfs_volume(false);
    let owner = ROOT.caller();

    vol.create(owner, &vol_path("target"), new_file())
        .expect("create the target");
    make_link(&vol, "link", "target");

    assert_eq!(
        vol.readlink(owner, &vol_path("link")),
        Ok(String::from("target"))
    );
}

#[test]
fn a_relative_target_carrying_dotdot_is_stored_as_typed() {
    // A target is *data*: nothing normalises it on the way in, so the
    // spelling that comes back is the spelling that went in.
    let vol = arxfs_volume(false);

    make_link(&vol, "up", "../elsewhere/x");
    assert_eq!(
        vol.readlink(ROOT.caller(), &vol_path("up")),
        Ok(String::from("../elsewhere/x"))
    );
}

#[test]
fn readlink_refuses_anything_that_is_not_a_link() {
    let vol = arxfs_volume(false);
    let owner = ROOT.caller();

    vol.create(owner, &vol_path("plain"), new_file())
        .expect("create");
    vol.mkdir(owner, &vol_path("dir"), new_dir())
        .expect("mkdir");

    // A name that is not a link has no target to read: the same domain
    // refusal for a file and for a directory.
    assert_eq!(
        vol.readlink(owner, &vol_path("plain")),
        Err(Errno::OutOfRange)
    );
    assert_eq!(
        vol.readlink(owner, &vol_path("dir")),
        Err(Errno::OutOfRange)
    );
    assert_eq!(
        vol.readlink(owner, &vol_path("absent")),
        Err(Errno::NotFound)
    );
}

#[test]
fn lstat_describes_the_link_and_stat_describes_its_target() {
    // `Keep` is POSIX `lstat`, `Follow` is POSIX `stat`. They must report
    // *different nodes*, not merely different sizes.
    let vol = arxfs_volume(false);
    let owner = ROOT.caller();

    vol.create(owner, &vol_path("target"), new_file())
        .expect("create the target");
    vol.write(owner, &vol_path("target"), 0, b"0123456789")
        .expect("write the target");
    make_link(&vol, "link", "target");

    let kept = vol
        .stat(owner, &vol_path("link"), FinalLink::Keep)
        .expect("lstat the link");
    assert_eq!(kept.kind, FileKind::Symlink);
    // A link's own length is the length of the path it stores.
    assert_eq!(kept.size, "target".len() as u64);

    let followed = vol
        .stat(owner, &vol_path("link"), FinalLink::Follow)
        .expect("stat the target");
    assert_eq!(followed.kind, FileKind::Regular);
    assert_eq!(followed.size, 10);
    assert_ne!(kept.id, followed.id);
    let target = vol
        .stat(owner, &vol_path("target"), FinalLink::Keep)
        .expect("lstat the target");
    assert_eq!(followed.id, target.id);
}

#[test]
fn a_dangling_link_is_describable_only_without_following() {
    let vol = arxfs_volume(false);

    // Nothing is resolved at creation, so a link may legitimately dangle.
    make_link(&vol, "dangling", "nowhere");

    assert_eq!(
        kind_of(&vol, "dangling", FinalLink::Keep),
        Ok(FileKind::Symlink)
    );
    assert_eq!(
        kind_of(&vol, "dangling", FinalLink::Follow),
        Err(Errno::NotFound)
    );
    assert_eq!(
        vol.readlink(ROOT.caller(), &vol_path("dangling")),
        Ok(String::from("nowhere"))
    );
}

#[test]
fn a_write_through_a_link_reaches_the_target_and_leaves_the_link_a_link() {
    let vol = arxfs_volume(false);
    let owner = ROOT.caller();

    vol.create(owner, &vol_path("target"), new_file())
        .expect("create the target");
    make_link(&vol, "link", "target");

    vol.write(owner, &vol_path("link"), 0, b"through")
        .expect("write through the link");

    let mut buf = [0u8; 16];
    let read = vol
        .read(owner, &vol_path("target"), 0, &mut buf)
        .expect("read the target");
    assert_eq!(&buf[..read], b"through");
    // A write never replaces the link.
    assert_eq!(
        kind_of(&vol, "link", FinalLink::Keep),
        Ok(FileKind::Symlink)
    );
    assert_eq!(
        vol.readlink(owner, &vol_path("link")),
        Ok(String::from("target"))
    );
}

#[test]
fn a_truncate_through_a_link_reaches_the_target() {
    let vol = arxfs_volume(false);
    let owner = ROOT.caller();

    vol.create(owner, &vol_path("target"), new_file())
        .expect("create the target");
    vol.write(owner, &vol_path("target"), 0, b"0123456789")
        .expect("write the target");
    make_link(&vol, "link", "target");

    vol.truncate(owner, &vol_path("link"), 4)
        .expect("truncate through the link");

    assert_eq!(
        vol.stat(owner, &vol_path("target"), FinalLink::Keep)
            .map(|info| info.size),
        Ok(4)
    );
    assert_eq!(
        kind_of(&vol, "link", FinalLink::Keep),
        Ok(FileKind::Symlink)
    );
}

#[test]
fn a_write_or_truncate_through_a_dangling_link_is_not_found_and_creates_nothing() {
    let vol = arxfs_volume(false);
    let owner = ROOT.caller();

    make_link(&vol, "dangling", "nowhere");

    assert_eq!(
        vol.write(owner, &vol_path("dangling"), 0, b"x"),
        Err(Errno::NotFound)
    );
    assert_eq!(
        vol.truncate(owner, &vol_path("dangling"), 0),
        Err(Errno::NotFound)
    );
    assert_eq!(
        kind_of(&vol, "nowhere", FinalLink::Keep),
        Err(Errno::NotFound)
    );
}

#[test]
fn a_create_through_a_dangling_link_creates_the_target() {
    // `open` with `O_CREAT` follows a final link, so creating "through" a
    // dangling one creates the file the link names. `O_EXCL` keeps the link,
    // an existing name.
    let vol = arxfs_volume(false);
    let owner = ROOT.caller();

    make_link(&vol, "dangling", "made");

    assert_eq!(
        vol.create(owner, &vol_path("dangling"), new_file()),
        Err(Errno::AlreadyExists)
    );
    vol.open(
        owner,
        &vol_path("dangling"),
        OpenFlags::CREATE.union(OpenFlags::WRITE),
        new_file(),
    )
    .expect("create through the dangling link");

    assert_eq!(
        kind_of(&vol, "made", FinalLink::Keep),
        Ok(FileKind::Regular)
    );
    assert_eq!(
        kind_of(&vol, "dangling", FinalLink::Keep),
        Ok(FileKind::Symlink)
    );
    assert_eq!(
        kind_of(&vol, "dangling", FinalLink::Follow),
        Ok(FileKind::Regular)
    );
}

#[test]
fn mkdir_over_a_link_is_already_exists_live_or_dangling() {
    // POSIX `mkdir` keeps the name as typed, so a link occupying it is an
    // occupied name — whatever it resolves to.
    let vol = arxfs_volume(false);
    let owner = ROOT.caller();

    vol.mkdir(owner, &vol_path("real"), new_dir())
        .expect("mkdir the target");
    make_link(&vol, "live", "real");
    make_link(&vol, "dead", "nowhere");

    assert_eq!(
        vol.mkdir(owner, &vol_path("live"), new_dir()),
        Err(Errno::AlreadyExists)
    );
    assert_eq!(
        vol.mkdir(owner, &vol_path("dead"), new_dir()),
        Err(Errno::AlreadyExists)
    );
    for name in ["live", "dead"] {
        assert_eq!(kind_of(&vol, name, FinalLink::Keep), Ok(FileKind::Symlink));
    }
}

#[test]
fn symlink_never_replaces_an_existing_name() {
    let vol = arxfs_volume(false);
    let owner = ROOT.caller();

    vol.create(owner, &vol_path("taken"), new_file())
        .expect("create");
    assert_eq!(
        vol.symlink(owner, &vol_path("taken"), "elsewhere"),
        Err(Errno::AlreadyExists)
    );
    assert_eq!(
        kind_of(&vol, "taken", FinalLink::Keep),
        Ok(FileKind::Regular)
    );
}

#[test]
fn unlink_removes_the_link_and_never_its_target() {
    let vol = arxfs_volume(false);
    let owner = ROOT.caller();

    vol.create(owner, &vol_path("target"), new_file())
        .expect("create the target");
    vol.write(owner, &vol_path("target"), 0, b"kept")
        .expect("write the target");
    make_link(&vol, "link", "target");

    vol.unlink(owner, &vol_path("link"), UnlinkFlags::empty())
        .expect("unlink the link");

    assert_eq!(kind_of(&vol, "link", FinalLink::Keep), Err(Errno::NotFound));
    let mut buf = [0u8; 8];
    let read = vol
        .read(owner, &vol_path("target"), 0, &mut buf)
        .expect("the target survived");
    assert_eq!(&buf[..read], b"kept");
}

#[test]
fn rename_moves_the_link_not_what_it_names() {
    let vol = arxfs_volume(false);
    let owner = ROOT.caller();

    vol.create(owner, &vol_path("target"), new_file())
        .expect("create the target");
    make_link(&vol, "link", "target");

    vol.rename(owner, &vol_path("link"), &vol_path("moved"))
        .expect("rename the link");

    assert_eq!(
        kind_of(&vol, "moved", FinalLink::Keep),
        Ok(FileKind::Symlink)
    );
    assert_eq!(
        vol.readlink(owner, &vol_path("moved")),
        Ok(String::from("target"))
    );
    assert_eq!(kind_of(&vol, "link", FinalLink::Keep), Err(Errno::NotFound));
    assert_eq!(
        kind_of(&vol, "target", FinalLink::Keep),
        Ok(FileKind::Regular)
    );
}

#[test]
fn a_cycle_is_refused_rather_than_walked() {
    // Resolution is bounded, so two links naming each other are refused with
    // the loop errno instead of running until something else gives out.
    let vol = arxfs_volume(false);

    make_link(&vol, "a", "b");
    make_link(&vol, "b", "a");

    assert_eq!(kind_of(&vol, "a", FinalLink::Follow), Err(Errno::LinkLoop));
    make_link(&vol, "self", "self");
    assert_eq!(
        kind_of(&vol, "self", FinalLink::Follow),
        Err(Errno::LinkLoop)
    );
    for name in ["a", "b", "self"] {
        assert_eq!(kind_of(&vol, name, FinalLink::Keep), Ok(FileKind::Symlink));
    }
}

#[test]
fn a_link_is_listed_as_a_link_by_its_parent() {
    let vol = arxfs_volume(false);
    let owner = ROOT.caller();

    vol.create(owner, &vol_path("target"), new_file())
        .expect("create the target");
    make_link(&vol, "link", "target");

    let listing = vol.list(owner, MOUNT).expect("list the volume root");
    let link = listing
        .iter()
        .find(|entry| entry.name == "link")
        .expect("the link is listed");
    // The stream reports each child's *own* kind.
    assert_eq!(link.kind, FileKind::Symlink);
}

#[test]
fn a_link_is_not_byte_readable() {
    // Its content is a path, reached only with `readlink`; a byte read fails
    // closed rather than handing the spelling out as file content.
    let vol = arxfs_volume(false);
    let owner = ROOT.caller();

    make_link(&vol, "link", "nowhere");

    let mut buf = [0u8; 16];
    assert_eq!(
        vol.read(owner, &vol_path("link"), 0, &mut buf),
        Err(Errno::NotFound)
    );
    assert_eq!(
        vol.open(
            owner,
            &vol_path("link"),
            OpenFlags::READ.union(OpenFlags::NO_FOLLOW),
            CreateMode::NONE,
        ),
        Err(Errno::LinkLoop)
    );
}

#[test]
fn an_interior_link_is_always_followed() {
    // A component used *as a directory* is resolved whatever the final
    // component's posture is.
    let vol = arxfs_volume(false);
    let owner = ROOT.caller();

    vol.mkdir(owner, &vol_path("real"), new_dir())
        .expect("mkdir");
    vol.create(owner, &vol_path("real/inner"), new_file())
        .expect("create inside it");
    vol.write(owner, &vol_path("real/inner"), 0, b"deep")
        .expect("write");
    make_link(&vol, "door", "real");

    for links in [FinalLink::Keep, FinalLink::Follow] {
        assert_eq!(kind_of(&vol, "door/inner", links), Ok(FileKind::Regular));
    }
    let mut buf = [0u8; 8];
    let read = vol
        .read(owner, &vol_path("door/inner"), 0, &mut buf)
        .expect("read through the interior link");
    assert_eq!(&buf[..read], b"deep");
}

#[test]
fn dotdot_in_a_target_is_resolved_physically() {
    // `..` in a link's target pops the directory the walk *really came
    // through*, never the one the spelling suggests. Collapsing it textually
    // is the classic symlink-escape bug, so a lexical resolution would
    // succeed here and reach a *different* file.
    let vol = arxfs_volume(false);
    let owner = ROOT.caller();

    vol.mkdir(owner, &vol_path("here"), new_dir())
        .expect("mkdir here");
    vol.mkdir(owner, &vol_path("there"), new_dir())
        .expect("mkdir there");
    vol.create(owner, &vol_path("prize"), new_file())
        .expect("create the root prize");
    vol.write(owner, &vol_path("prize"), 0, b"physical")
        .expect("write the root prize");
    vol.create(owner, &vol_path("here/prize"), new_file())
        .expect("create the decoy");
    vol.write(owner, &vol_path("here/prize"), 0, b"lexical!")
        .expect("write the decoy");

    // `here/door` names `there`; `there/out` names its parent's `prize`.
    make_link(&vol, "here/door", "/there");
    make_link(&vol, "there/out", "../prize");

    let mut buf = [0u8; 16];
    let read = vol
        .read(owner, &vol_path("here/door/out"), 0, &mut buf)
        .expect("read through the link chain");
    assert_eq!(
        &buf[..read],
        b"physical",
        "`..` popped the spelled parent instead of the real one"
    );
}

#[test]
fn a_link_cannot_escape_the_volume_that_stores_it() {
    // An absolute target resolves against the *mounted volume's* own root, so
    // a link on this volume cannot name the system tree above it — stricter
    // than POSIX, deliberately.
    let vol = arxfs_volume(false);

    vol.create(ROOT.caller(), &vol_path("inside"), new_file())
        .expect("create inside the volume");
    make_link(&vol, "escape", "/System/Security/Users");
    assert_eq!(
        kind_of(&vol, "escape", FinalLink::Follow),
        Err(Errno::NotFound)
    );
    // Climbing out of the volume root with `..` lands back at the root, as
    // POSIX specifies for `/..`.
    make_link(&vol, "climb", "/../../inside");
    assert_eq!(
        kind_of(&vol, "climb", FinalLink::Follow),
        Ok(FileKind::Regular)
    );
}

#[test]
fn creating_a_link_needs_a_writable_mount_and_an_authorised_parent() {
    // The call authorises exactly one thing: the right to create a name in
    // the link's own parent. It grants nothing over what the target names.
    let read_only = arxfs_volume(true);
    assert_eq!(
        read_only.symlink(ROOT.caller(), &vol_path("nope"), "target"),
        Err(Errno::ReadOnly)
    );

    let vol = arxfs_volume(false);
    assert_eq!(
        vol.symlink(STRANGER.caller(), &vol_path("nope"), "target"),
        Err(Errno::PermissionDenied)
    );
    assert_eq!(kind_of(&vol, "nope", FinalLink::Keep), Err(Errno::NotFound));
}

#[test]
fn an_unwalkable_target_is_refused_rather_than_stored() {
    // A target's *grammar* is checked before it is written, so a link that
    // could only ever fail is never created. Parsing is not resolving: `..`
    // and relative spellings stay legal.
    let vol = arxfs_volume(false);

    let over_long = "x".repeat(tairix_abi::FS_SYMLINK_MAX + 1);
    for (target, why) in [("", "empty"), (over_long.as_str(), "over the bound")] {
        assert!(
            vol.symlink(ROOT.caller(), &vol_path("bad"), target)
                .is_err(),
            "a {why} target must be refused"
        );
        assert_eq!(
            kind_of(&vol, "bad", FinalLink::Keep),
            Err(Errno::NotFound),
            "a refused {why} target must leave no name behind"
        );
    }
}
