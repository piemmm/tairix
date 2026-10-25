//! Host tests for the in-memory test driver's own contract: the parent it
//! reports, the nodes a byte write reaches, the holds it keeps, and the
//! numbers it reuses.

use super::*;

use tairix_abi::driver::filesystem::listing::listed;
use tairix_abi::driver::filesystem::{SecurityAcl, SecuritySubject};

/// A directory's parent is the directory holding its name, the root's is
/// itself, a file has none to report, and a removed directory names nothing.
#[test]
fn parent_climbs_to_the_root_and_stops_there() {
    let mut fs = RwMockFs::new();
    let root = fs.root();
    let outer = fs
        .create(root, b"outer", NodeKind::Directory, &ADMIN_RECORD)
        .expect("mkdir");
    let inner = fs
        .create(outer, b"inner", NodeKind::Directory, &ADMIN_RECORD)
        .expect("mkdir");
    let file = fs
        .create(inner, b"file", NodeKind::RegularFile, &ADMIN_RECORD)
        .expect("create");
    assert_eq!(fs.parent(inner), Ok(outer));
    assert_eq!(fs.parent(outer), Ok(root));
    assert_eq!(fs.parent(root), Ok(root));
    assert_eq!(fs.parent(file), Err(DriverError::Unsupported));
    fs.remove(inner, b"file").expect("unlink");
    fs.remove(outer, b"inner").expect("rmdir");
    assert_eq!(fs.parent(inner), Err(DriverError::NotFound));
}

/// A moved directory's parent is the directory it moved to.
#[test]
fn parent_follows_a_moved_directory() {
    let mut fs = RwMockFs::new();
    let root = fs.root();
    let from = fs
        .create(root, b"from", NodeKind::Directory, &ADMIN_RECORD)
        .expect("mkdir");
    let to = fs
        .create(root, b"to", NodeKind::Directory, &ADMIN_RECORD)
        .expect("mkdir");
    let moved = fs
        .create(from, b"moved", NodeKind::Directory, &ADMIN_RECORD)
        .expect("mkdir");
    fs.rename(from, b"moved", to, b"moved").expect("rename");
    assert_eq!(fs.parent(moved), Ok(to));
}

/// A byte write names its node, and a node no name and no hold reaches is one
/// no call reaches: neither its bytes nor its record are served.
#[test]
fn a_node_whose_last_name_went_unheld_answers_nothing() {
    let mut fs = RwMockFs::new();
    let root = fs.root();
    let file = fs
        .create(root, b"file", NodeKind::RegularFile, &ADMIN_RECORD)
        .expect("create");
    fs.write_all(file, 0, b"body").expect("write");
    fs.remove(root, b"file").expect("unlink");
    assert_eq!(fs.write_at(file, 0, b"x"), Err(DriverError::NotFound));
    assert_eq!(fs.truncate(file, 0), Err(DriverError::NotFound));
    assert_eq!(fs.punch(file, 0, 1), Err(DriverError::NotFound));
    let mut body = [0u8; 4];
    assert_eq!(fs.read_at(file, 0, &mut body), Err(DriverError::NotFound));
    assert_eq!(fs.node_info(file), Err(DriverError::NotFound));
    assert_eq!(fs.security(file), Err(DriverError::NotFound));
    assert_eq!(fs.pin(file), Err(DriverError::NotFound));
    assert_eq!(fs.write_at(root, 0, b"x"), Err(DriverError::Unsupported));
}

/// A held node outlives its last name with its bytes and its storage, takes
/// byte writes and reports no names, refuses a new name, and its last unpin
/// frees it.
#[test]
fn a_held_node_outlives_its_last_name_until_its_last_unpin() {
    let mut fs = RwMockFs::new();
    let root = fs.root();
    let file = fs
        .create(root, b"file", NodeKind::RegularFile, &ADMIN_RECORD)
        .expect("create");
    fs.write_all(file, 0, &[7; 2048]).expect("write");
    let free = fs.stats().expect("stats").free_blocks;
    let held = fs.pin(file).expect("pin");
    assert_eq!(held, file, "a node keeps its own id under a hold");
    fs.remove(root, b"file").expect("unlink");
    assert_eq!(fs.lookup(root, b"file"), Err(DriverError::NotFound));
    assert_eq!(fs.node_info(held).expect("held").nlink, 0);
    assert_eq!(fs.write_at(held, 0, b"new"), Ok(3));
    let mut body = [0u8; 4];
    assert_eq!(fs.read_at(held, 0, &mut body), Ok(4));
    assert_eq!(&body, b"new\x07");
    assert_eq!(fs.security(held), Ok(ADMIN_RECORD));
    assert_eq!(fs.link(root, b"again", held), Err(DriverError::NotFound));
    assert_eq!(fs.stats().expect("stats").free_blocks, free);
    assert_eq!(fs.unpin(held), Ok(Unpinned::Freed));
    assert_eq!(fs.stats().expect("stats").free_blocks, free + 4);
    assert_eq!(fs.node_info(held), Err(DriverError::NotFound));
    assert_eq!(fs.read_at(held, 0, &mut body), Err(DriverError::NotFound));
}

/// Holds count, a lookup and a listing answer the held id, and an unpin with
/// no hold outstanding is refused.
#[test]
fn holds_count_and_each_answers_the_same_id() {
    let mut fs = RwMockFs::new();
    let root = fs.root();
    let file = fs
        .create(root, b"file", NodeKind::RegularFile, &ADMIN_RECORD)
        .expect("create");
    let held = fs.pin(file).expect("pin");
    assert_eq!(fs.pin(file), Ok(held));
    assert_eq!(fs.lookup(root, b"file"), Ok(held));
    let entries = listed(&mut fs, root, 0, &[]).expect("lists");
    assert!(entries
        .iter()
        .any(|(entry, name)| name.as_slice() == b"file" && entry.node == held));
    assert_eq!(fs.unpin(held), Ok(Unpinned::Held));
    assert_eq!(fs.unpin(held), Ok(Unpinned::Released));
    assert_eq!(fs.unpin(held), Err(DriverError::NotFound));
    assert_eq!(fs.pin(NodeId::from_raw(99)), Err(DriverError::NotFound));
}

/// A held number is never given to another node: a create after the held
/// node's removal takes another, and the held one still reads its bytes.
#[test]
fn a_held_number_is_never_handed_out() {
    let mut fs = RwMockFs::new().with_reused_numbers();
    let root = fs.root();
    let held = fs
        .create(root, b"held", NodeKind::RegularFile, &ADMIN_RECORD)
        .expect("create");
    fs.write_all(held, 0, b"kept").expect("write");
    assert_eq!(fs.pin(held), Ok(held));
    fs.remove(root, b"held").expect("unlink");
    let newcomer = fs
        .create(root, b"newcomer", NodeKind::RegularFile, &ADMIN_RECORD)
        .expect("create");
    assert_ne!(newcomer, held);
    let mut body = [0u8; 8];
    let read = fs.read_at(held, 0, &mut body).expect("read");
    assert_eq!(&body[..read], b"kept");
    assert_eq!(fs.unpin(held), Ok(Unpinned::Freed));
    let after = fs
        .create(root, b"after", NodeKind::RegularFile, &ADMIN_RECORD)
        .expect("create");
    assert_eq!(after, held, "a freed number is the next node's");
}

/// A removed directory that is still held takes no entries and lists
/// nothing, and its last unpin frees it.
#[test]
fn a_removed_held_directory_takes_no_entries() {
    let mut fs = RwMockFs::new();
    let root = fs.root();
    let dir = fs
        .create(root, b"dir", NodeKind::Directory, &ADMIN_RECORD)
        .expect("mkdir");
    let file = fs
        .create(root, b"file", NodeKind::RegularFile, &ADMIN_RECORD)
        .expect("create");
    assert_eq!(fs.pin(dir), Ok(dir));
    fs.remove(root, b"dir").expect("rmdir");
    assert_eq!(fs.lookup(dir, b"x"), Err(DriverError::NotFound));
    assert_eq!(fs.parent(dir), Err(DriverError::NotFound));
    assert_eq!(
        fs.create(dir, b"x", NodeKind::RegularFile, &ADMIN_RECORD),
        Err(DriverError::NotFound)
    );
    assert_eq!(
        fs.create_link(dir, b"x", b"/file", &ADMIN_RECORD),
        Err(DriverError::NotFound)
    );
    assert_eq!(fs.link(dir, b"x", file), Err(DriverError::NotFound));
    assert_eq!(
        fs.rename(root, b"file", dir, b"x"),
        Err(DriverError::NotFound)
    );
    assert!(listed(&mut fs, dir, 0, &[]).expect("lists").is_empty());
    assert_eq!(fs.node_info(dir).expect("held").nlink, 0);
    assert_eq!(fs.unpin(dir), Ok(Unpinned::Freed));
    assert_eq!(fs.node_info(dir), Err(DriverError::NotFound));
}

/// Releasing every hold frees each held node no name reaches and leaves the
/// named ones be.
#[test]
fn releasing_every_hold_frees_each_nameless_held_node() {
    let mut fs = RwMockFs::new();
    let root = fs.root();
    let gone = fs
        .create(root, b"gone", NodeKind::RegularFile, &ADMIN_RECORD)
        .expect("create");
    let kept = fs
        .create(root, b"kept", NodeKind::RegularFile, &ADMIN_RECORD)
        .expect("create");
    fs.pin(gone).expect("pin");
    fs.pin(kept).expect("pin");
    fs.remove(root, b"gone").expect("unlink");
    assert_eq!(fs.release_pins(), Ok(()));
    assert_eq!(fs.node_info(gone), Err(DriverError::NotFound));
    assert_eq!(fs.node_info(kept).expect("named").nlink, 1);
    assert_eq!(
        fs.unpin(kept),
        Err(DriverError::NotFound),
        "its hold went with the rest"
    );
}

/// A held node keeps its id through the writes and renames that move an
/// unheld one.
#[test]
fn a_held_node_is_never_renumbered() {
    let mut fs = RwMockFs::new().with_renumbering_writes();
    let root = fs.root();
    let file = fs
        .create(root, b"file", NodeKind::RegularFile, &ADMIN_RECORD)
        .expect("create");
    let held = fs.pin(file).expect("pin");
    fs.write_at(held, 0, b"body").expect("write");
    assert_eq!(fs.lookup(root, b"file"), Ok(held));

    let mut fs = RwMockFs::new().with_renumbering_renames();
    let root = fs.root();
    let file = fs
        .create(root, b"before", NodeKind::RegularFile, &ADMIN_RECORD)
        .expect("create");
    let held = fs.pin(file).expect("pin");
    fs.rename(root, b"before", root, b"after").expect("rename");
    assert_eq!(fs.lookup(root, b"after"), Ok(held));
    assert_eq!(fs.unpin(held), Ok(Unpinned::Released));
}

/// A create stores the whole record it was handed.
#[test]
fn a_create_stores_the_record_it_was_handed() {
    let mut fs = RwMockFs::new();
    let root = fs.root();
    let mut record = NodeSecurity::new(0o640, 42, 43);
    record.required_cap = Some(tairix_abi::CapabilityId::AUDIT_READ);
    record
        .push_acl(SecurityAcl {
            subject: SecuritySubject::User(44),
            perms: 0b100,
        })
        .expect("acl");
    let file = fs
        .create(root, b"file", NodeKind::RegularFile, &record)
        .expect("create");
    let link = fs
        .create_link(root, b"link", b"/file", &record)
        .expect("link");
    assert_eq!(fs.security(file), Ok(record));
    assert_eq!(fs.security(link), Ok(record));
}

/// A rename gives up the node's last name only while it moves, so the next
/// node made never takes the moved node's number and its bytes.
#[test]
fn a_renamed_nodes_number_is_not_given_to_the_next_node() {
    let mut fs = RwMockFs::new().with_reused_numbers();
    let root = fs.root();
    let moved = fs
        .create(root, b"before", NodeKind::RegularFile, &ADMIN_RECORD)
        .expect("create");
    fs.write_all(moved, 0, b"kept").expect("write");
    fs.rename(root, b"before", root, b"after").expect("rename");
    let newcomer = fs
        .create(root, b"newcomer", NodeKind::RegularFile, &ADMIN_RECORD)
        .expect("create");
    assert_ne!(newcomer, moved);
    let mut body = [0u8; 8];
    let read = fs.read_at(moved, 0, &mut body).expect("read");
    assert_eq!(&body[..read], b"kept");
}

/// Where writes renumber, a write moves the file to a number its name then
/// answers, and the number it left reaches nothing a write can change.
#[test]
fn a_renumbering_write_moves_the_file_its_name_follows() {
    let mut fs = RwMockFs::new().with_renumbering_writes();
    assert_eq!(fs.node_identity(), NodeIdentity::ContentDerived);
    let root = fs.root();
    let file = fs
        .create(root, b"file", NodeKind::RegularFile, &ADMIN_RECORD)
        .expect("create");
    fs.write_at(file, 0, b"body").expect("write");
    let moved = fs.lookup(root, b"file").expect("resolves");
    assert_ne!(moved, file);
    assert_eq!(fs.write_at(file, 0, b"x"), Err(DriverError::NotFound));
    let mut body = [0u8; 8];
    let read = fs.read_at(moved, 0, &mut body).expect("read");
    assert_eq!(&body[..read], b"body");
}

/// Where renames renumber, a renamed file answers a new number at its new
/// name and the next node made takes the number it left.
#[test]
fn a_renumbering_rename_frees_the_old_number_for_the_next_node() {
    let mut fs = RwMockFs::new()
        .with_renumbering_renames()
        .with_reused_numbers();
    assert_eq!(fs.node_identity(), NodeIdentity::Stable);
    let root = fs.root();
    let file = fs
        .create(root, b"before", NodeKind::RegularFile, &ADMIN_RECORD)
        .expect("create");
    fs.write_all(file, 0, b"kept").expect("write");
    assert_eq!(
        fs.lookup(root, b"before"),
        Ok(file),
        "a write keeps the number"
    );
    fs.rename(root, b"before", root, b"after").expect("rename");
    let moved = fs.lookup(root, b"after").expect("resolves");
    assert_ne!(moved, file);
    let newcomer = fs
        .create(root, b"newcomer", NodeKind::RegularFile, &ADMIN_RECORD)
        .expect("create");
    assert_eq!(newcomer, file);
    let mut body = [0u8; 8];
    let read = fs.read_at(moved, 0, &mut body).expect("read");
    assert_eq!(&body[..read], b"kept");
}
