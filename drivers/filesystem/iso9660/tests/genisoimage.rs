//! The driver against discs `genisoimage 1.1.11` mastered from one tree:
//! Rock Ridge and Joliet together, the primary tree alone, Joliet alone, and
//! a UDF bridge whose Rock Ridge tree is mounted.

// The fixtures are a few hundred KiB, so every size fits the narrower field
// it is compared with.
#![allow(clippy::cast_possible_truncation)]

#[path = "../../ext4/tests/support/sparse.rs"]
mod sparse;

use tairix_abi::driver::block::{Block, BlockGeometry};
use tairix_abi::driver::filesystem::listing::{first_listed, listed};
use tairix_abi::driver::filesystem::{FilesystemRead, FilesystemSecurity, NodeId, NodeKind};
use tairix_abi::DriverError;
use tairix_drv_fs_iso9660::Iso9660;

const RR_JOLIET: &[u8] = include_bytes!("fixtures/rr-joliet.sparse");
const PLAIN: &[u8] = include_bytes!("fixtures/plain.sparse");
const JOLIET: &[u8] = include_bytes!("fixtures/joliet.sparse");
const BRIDGE: &[u8] = include_bytes!("fixtures/bridge.sparse");

/// A disc in its drive's 2048-byte blocks.
struct Disc(Vec<u8>);

impl Block for Disc {
    fn geometry(&self) -> Result<BlockGeometry, DriverError> {
        Ok(BlockGeometry {
            block_size: 2048,
            block_count: (self.0.len() / 2048) as u64,
        })
    }

    fn read_blocks(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        let at = lba as usize * 2048;
        buf.copy_from_slice(
            self.0
                .get(at..at + buf.len())
                .ok_or(DriverError::OutOfRange)?,
        );
        Ok(())
    }

    fn write_blocks(&mut self, _: u64, _: &[u8]) -> Result<(), DriverError> {
        Err(DriverError::ReadOnly)
    }

    fn flush(&mut self) -> Result<(), DriverError> {
        Ok(())
    }
}

fn mount(fixture: &[u8]) -> Iso9660<Disc> {
    Iso9660::open(Disc(sparse::sparse(fixture))).expect("mounts")
}

fn names(fs: &mut Iso9660<Disc>, dir: NodeId) -> Vec<String> {
    listed(fs, dir, 0, &[])
        .expect("lists")
        .into_iter()
        .map(|(_, name)| String::from_utf8(name).expect("UTF-8"))
        .collect()
}

fn walk(fs: &mut Iso9660<Disc>, path: &[&str]) -> NodeId {
    path.iter().fold(fs.root(), |dir, name| {
        fs.lookup(dir, name.as_bytes())
            .unwrap_or_else(|err| panic!("{name}: {err:?}"))
    })
}

fn contents(fs: &mut Iso9660<Disc>, path: &[&str]) -> Vec<u8> {
    let node = walk(fs, path);
    let mut out = vec![0u8; fs.node_info(node).expect("info").size as usize];
    let mut done = 0;
    while done < out.len() {
        let n = fs
            .read_at(node, done as u64, &mut out[done..])
            .expect("reads");
        assert_ne!(n, 0, "short of its size");
        done += n;
    }
    out
}

fn pattern() -> Vec<u8> {
    (0..5000u32).map(|i| ((i * 31 + 7) % 256) as u8).collect()
}

#[test]
fn a_rock_ridge_disc_is_read_as_posix_wrote_it() {
    let mut fs = mount(RR_JOLIET);
    let root = fs.root();
    assert_eq!(
        names(&mut fs, root),
        [
            "absolute-link",
            "Café.txt",
            "d1",
            "empty",
            "link-to-readme",
            "many",
            "Mixed Case Long Name.txt",
            "pattern.bin",
            "README.TXT",
            "rr_moved",
        ]
    );
    assert_eq!(contents(&mut fs, &["README.TXT"]), b"hello\n");
    assert_eq!(contents(&mut fs, &["Café.txt"]), b"cafe\n");
    assert_eq!(contents(&mut fs, &["pattern.bin"]), pattern());
    assert!(contents(&mut fs, &["empty"]).is_empty());
    let pattern = walk(&mut fs, &["pattern.bin"]);
    assert_eq!(fs.security(pattern).expect("recorded").mode, 0o640);

    let mut target = [0u8; 64];
    for (link, expected) in [
        ("link-to-readme", "README.TXT"),
        ("absolute-link", "/usr/bin/env"),
    ] {
        let node = walk(&mut fs, &[link]);
        assert_eq!(fs.node_info(node).expect("info").kind, NodeKind::Symlink);
        let len = fs.read_link(node, &mut target).expect("a link");
        assert_eq!(&target[..len], expected.as_bytes());
    }
}

#[test]
fn a_directory_rock_ridge_relocated_is_found_at_its_depth() {
    let mut fs = mount(RR_JOLIET);
    let path = [
        "d1", "d2", "d3", "d4", "d5", "d6", "d7", "d8", "d9", "leaf.txt",
    ];
    assert_eq!(contents(&mut fs, &path), b"deep\n");
    let moved = walk(&mut fs, &["rr_moved"]);
    assert!(
        names(&mut fs, moved).is_empty(),
        "the relocated record is not listed twice"
    );
}

/// A directory's parent is the one its `..` record names, found among the
/// entries of the directory the parent's own `..` names, and the root's is
/// the root, in each tree a disc records.
#[test]
fn a_directorys_parent_is_the_one_its_dot_dot_names() {
    for fixture in [RR_JOLIET, PLAIN, JOLIET] {
        let mut fs = mount(fixture);
        let root = fs.root();
        assert_eq!(fs.parent(root), Ok(root));
        let d1 = walk(&mut fs, &["d1"]);
        let d2 = walk(&mut fs, &["d1", "d2"]);
        assert_eq!(fs.parent(d1), Ok(root));
        assert_eq!(fs.parent(d2), Ok(d1));
        let readme = walk(&mut fs, &["README.TXT"]);
        assert_eq!(fs.parent(readme), Err(DriverError::Unsupported));
        assert_eq!(fs.node_info(d2).expect("info").ino, d2.raw());
    }
}

/// A directory Rock Ridge relocated has `rr_moved` as its `..`, and the
/// `PL` entry there names the directory it belongs in: that is its parent,
/// and its children climb through it to there.
#[test]
fn a_relocated_directorys_parent_is_the_directory_it_belongs_in() {
    let mut fs = mount(RR_JOLIET);
    let path = ["d1", "d2", "d3", "d4", "d5", "d6", "d7", "d8", "d9"];
    let d7 = walk(&mut fs, &path[..7]);
    let d8 = walk(&mut fs, &path[..8]);
    let d9 = walk(&mut fs, &path);
    assert_eq!(fs.parent(d8), Ok(d7));
    assert_eq!(fs.parent(d9), Ok(d8));
    let root = fs.root();
    let moved = walk(&mut fs, &["rr_moved"]);
    assert_eq!(fs.parent(moved), Ok(root));
}

#[test]
fn a_directory_of_several_sectors_lists_whole_and_resumes() {
    let mut fs = mount(RR_JOLIET);
    let many = walk(&mut fs, &["many"]);
    let expected: Vec<String> = (0..30).map(|i| format!("file{i:02}.dat")).collect();
    assert_eq!(names(&mut fs, many), expected);
    let (mut cursor, mut after, mut resumed) = (0, Vec::new(), Vec::new());
    while let Some((entry, name)) = first_listed(&mut fs, many, cursor, &after).expect("lists") {
        cursor = entry.next_cursor;
        resumed.push(String::from_utf8(name.clone()).expect("UTF-8"));
        after = name;
    }
    assert_eq!(resumed, expected);
    assert_eq!(contents(&mut fs, &["many", "file17.dat"]), b"file 17\n");
}

#[test]
fn a_disc_with_only_the_primary_tree_lists_its_d_character_names() {
    let mut fs = mount(PLAIN);
    let root = fs.root();
    assert_eq!(
        names(&mut fs, root),
        [
            "CAF__.TXT",
            "D1",
            "EMPTY",
            "MANY",
            "MIXED_CA.TXT",
            "PATTERN.BIN",
            "README.TXT"
        ]
    );
    assert_eq!(contents(&mut fs, &["readme.txt"]), b"hello\n");
    assert_eq!(contents(&mut fs, &["pattern.bin"]), pattern());
    let readme = walk(&mut fs, &["README.TXT"]);
    assert!(!fs.records_ownership(readme).expect("node"));
}

#[test]
fn a_joliet_disc_lists_its_unicode_names() {
    let mut fs = mount(JOLIET);
    let root = fs.root();
    let listed = names(&mut fs, root);
    for name in [
        "Café.txt",
        "Mixed Case Long Name.txt",
        "pattern.bin",
        "README.TXT",
    ] {
        assert!(listed.iter().any(|n| n == name), "{name} in {listed:?}");
    }
    assert_eq!(
        contents(&mut fs, &["CAFé.TXT"]),
        b"cafe\n",
        "case-blind in ASCII"
    );
    assert_eq!(
        fs.lookup(root, "CAFÉ.TXT".as_bytes()),
        Err(DriverError::NotFound),
        "and only ASCII"
    );
}

#[test]
fn a_bridge_disc_with_rock_ridge_is_mounted_from_its_iso_9660_tree() {
    let mut fs = mount(BRIDGE);
    assert_eq!(contents(&mut fs, &["Mixed Case Long Name.txt"]), b"mixed\n");
    let mut target = [0u8; 64];
    let link = walk(&mut fs, &["link-to-readme"]);
    let len = fs.read_link(link, &mut target).expect("a link");
    assert_eq!(&target[..len], b"README.TXT");
}
