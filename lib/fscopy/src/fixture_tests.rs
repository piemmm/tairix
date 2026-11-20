use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::{Access, Errno, NodeExpect, RenameFlags, UnlinkFlags};

use super::{resolve, Tree, USER};
use crate::data::{Source, Target};
use crate::tree::{Creation, Dir, FileAccess, Kind, Node, Roots, WriteAt};

#[test]
fn a_reached_file_is_opened_through_a_link_and_made_or_emptied_as_asked() {
    let tree = Tree::new()
        .dir("/s")
        .file("/s/f", b"held")
        .link("/s/l", "f")
        .dir("/s/d");
    let held = tree.open_dir("/s", false).expect("opens");
    let mut both = held
        .open_reached("l", FileAccess::Both, Creation::Existing, 0o644)
        .expect("through the link");
    let mut read = [0; 4];
    assert_eq!(both.read_at(0, &mut read), Ok(4));
    assert_eq!(&read, b"held");
    assert_eq!(both.write_at(4, b"!"), Ok(1));
    assert_eq!(tree.bytes("/s/f"), Some(b"held!".to_vec()));
    assert_eq!(
        held.open_reached("f", FileAccess::Write, Creation::New, 0o644)
            .map(drop),
        Err(Errno::AlreadyExists)
    );
    assert_eq!(
        held.open_reached("gone", FileAccess::Read, Creation::Existing, 0o644)
            .map(drop),
        Err(Errno::NotFound)
    );
    assert_eq!(
        held.open_reached("d", FileAccess::Read, Creation::Existing, 0o644)
            .map(drop),
        Err(Errno::IsADirectory)
    );
    held.open_reached("f", FileAccess::Write, Creation::Truncate, 0o644)
        .expect("emptied");
    assert_eq!(tree.bytes("/s/f"), Some(Vec::new()));
    held.open_reached("new", FileAccess::Write, Creation::New, 0o644)
        .expect("made");
    assert!(tree.exists("/s/new"));
}

#[test]
fn a_read_only_file_is_reached_to_read_alone_and_a_directory_asks_of_itself() {
    let tree = Tree::new()
        .dir("/s")
        .file("/s/f", b"held")
        .with_mode("/s/f", 0o444)
        .with_mode("/s", 0o555)
        .checking_permissions();
    let held = tree.open_dir("/s", false).expect("opens");
    held.open_reached("f", FileAccess::Read, Creation::Existing, 0o644)
        .expect("readable");
    assert_eq!(
        held.open_reached("f", FileAccess::Both, Creation::Existing, 0o644)
            .map(drop),
        Err(Errno::PermissionDenied)
    );
    assert_eq!(
        held.access("", Access::WRITE, false),
        Err(Errno::PermissionDenied)
    );
    assert_eq!(held.access("", Access::READ, false), Ok(()));
    assert_eq!(
        held.access("f", Access::WRITE, true),
        Err(Errno::PermissionDenied)
    );
}

#[test]
fn a_listing_stops_at_the_most_asked_for() {
    let tree = Tree::new()
        .dir("/s")
        .file("/s/a", b"")
        .file("/s/b", b"")
        .file("/s/c", b"");
    let held = tree.open_dir("/s", false).expect("opens");
    assert_eq!(held.entries(2).map(|entries| entries.len()), Ok(2));
    assert_eq!(held.entries(usize::MAX).map(|entries| entries.len()), Ok(3));
}

#[test]
fn a_held_directory_is_its_node_wherever_its_name_goes() {
    let tree = Tree::new().dir("/a").file("/a/f", b"held");
    let held = tree.open_dir("/a", false).expect("opens");
    tree.rename("/a", "/b");
    tree.relink("/a", "/b");
    let names: Vec<_> = held
        .entries(usize::MAX)
        .expect("lists")
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    assert_eq!(names, ["f"]);
    assert_eq!(held.stat().map(|stat| stat.id).ok(), tree.id("/b"));
}

/// Each volume counts what its own files hold, a stored run at a time.
#[test]
fn a_volume_accounts_for_the_files_on_it_alone() {
    let tree = Tree::new()
        .dir("/a")
        .file("/a/f", &[1; 1000])
        .whole_second_volume("/other");
    let home = tree.open_dir("/a", false).expect("opens");
    let other = tree.open_dir("/other", false).expect("opens");
    let before = home.volume_stats().expect("stats");
    assert!(before.is_consistent());
    assert_eq!(
        before.total_blocks - before.free_blocks,
        2,
        "1000 bytes take two blocks"
    );
    let apart = other.volume_stats().expect("stats");
    assert_eq!(apart.free_blocks, apart.total_blocks, "nothing is on it");
    let mut file = home.create_file("g", 0o644).expect("made");
    file.write_all_at(0, &[2; 512]).expect("written");
    assert_eq!(
        home.volume_stats().expect("stats").free_blocks,
        before.free_blocks - 1
    );
}

/// A file grows only into the room its volume has, so a hostile length is
/// refused as a full volume refuses it rather than held in memory.
#[test]
fn a_file_grown_past_its_volume_is_refused_as_a_full_volume_refuses_it() {
    let tree = Tree::new().dir("/a").file("/a/f", &[1; 1000]);
    let home = tree.open_dir("/a", false).expect("opens");
    let stats = home.volume_stats().expect("stats");
    let room = stats.free_blocks * u64::from(stats.block_size);
    let mut file = home.create_file("g", 0o644).expect("made");
    assert_eq!(file.set_len(room + 1), Err(Errno::NoSpace));
    assert_eq!(file.set_len(u64::MAX), Err(Errno::NoSpace));
    assert_eq!(file.write_all_at(room, b"x"), Err(Errno::NoSpace));
    assert_eq!(
        file.write_all_at(u64::MAX, b"x"),
        Err(Errno::LengthOutOfRange)
    );
    assert_eq!(tree.bytes("/a/g"), Some(Vec::new()), "nothing was taken");
    file.set_len(room).expect("the room it has");
    assert_eq!(home.volume_stats().expect("stats").free_blocks, 0);
    file.set_len(16).expect("shrunk");
    let mut read = home
        .open_reached("f", FileAccess::Read, Creation::Existing, 0o644)
        .expect("readable");
    assert_eq!(read.set_len(room * 2), Err(Errno::PermissionDenied));
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

/// A copy's times are its source's as nearly as the copy's volume keeps
/// them, answered exactly where they are kept exactly.
#[test]
fn a_copys_times_are_its_sources_as_nearly_as_its_volume_keeps_them() {
    use tairix_abi::time::Time64;
    let fine = Time64::new(-1_000, 750_000_000).expect("canonical");
    let tree = Tree::new()
        .file("/f", b"x")
        .whole_second_volume("/coarse")
        .file("/coarse/g", b"y");
    let times = tairix_abi::NodeTimes {
        accessed: fine,
        modified: fine,
        ..tairix_abi::NodeTimes::default()
    };
    let exact = tree
        .open_dir("/", false)
        .expect("opens")
        .open_file("f", false)
        .expect("opens");
    assert_eq!(
        crate::copy_times(&times, &exact),
        Ok(tairix_abi::TimesKept::Exactly)
    );
    assert_eq!(tree.times("/f"), Some((fine, fine)));
    let coarse = tree
        .open_dir("/coarse", false)
        .expect("opens")
        .open_file("g", false)
        .expect("opens");
    assert_eq!(
        crate::copy_times(&times, &coarse),
        Ok(tairix_abi::TimesKept::Nearest)
    );
    let held = Time64::from_secs(-999);
    assert_eq!(
        tree.times("/coarse/g"),
        Some((held, held)),
        "the nearest second"
    );
}
