// Test images are a few MiB, so every offset and length fits the narrower
// field it is compared with.
#![allow(clippy::cast_possible_truncation)]

use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::driver::block::{Block, BlockGeometry};
use tairix_abi::driver::filesystem::listing::listed;
use tairix_abi::driver::filesystem::{
    FilesystemAttrs, FilesystemRead, FilesystemSecurity, FilesystemStats, FilesystemWrite,
    NameMatching, NodeId, NodeKind, NodeSecurity, Unpinned,
};
use tairix_abi::DriverError;

use super::Iso9660;

use tairix_test_iso9660_image as master;

use master::{apple, archimedes, rr, Disc, Kind, Node, ASSOCIATED, PROTECTION};

/// An in-memory device over an image, in 512-byte blocks.
struct Image(Vec<u8>);

impl Block for Image {
    fn geometry(&self) -> Result<BlockGeometry, DriverError> {
        Ok(BlockGeometry {
            block_size: 512,
            block_count: (self.0.len() / 512) as u64,
        })
    }

    fn read_blocks(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        let at = lba as usize * 512;
        buf.copy_from_slice(
            self.0
                .get(at..at + buf.len())
                .ok_or(DriverError::OutOfRange)?,
        );
        Ok(())
    }

    fn write_blocks(&mut self, _lba: u64, _buf: &[u8]) -> Result<(), DriverError> {
        Err(DriverError::ReadOnly)
    }

    fn flush(&mut self) -> Result<(), DriverError> {
        Ok(())
    }
}

fn mount(disc: &Disc) -> Iso9660<Image> {
    Iso9660::open(Image(master::master(disc).image)).expect("mounts")
}

fn disc(children: Vec<Node>) -> Disc {
    Disc {
        volume_id: b"TEST".to_vec(),
        root: Node::dir(&[], children),
        ..Disc::default()
    }
}

/// A Rock Ridge disc: the root's `.` carries the indicator and `PX`.
fn rock_ridge(children: Vec<Node>) -> Disc {
    let mut root = Node::dir(&[], children);
    if let Kind::Dir { dot_use, .. } = &mut root.kind {
        *dot_use = rr::root(0);
        dot_use.extend(rr::px(0o040_755, 2, 0, 0));
    }
    Disc {
        volume_id: b"ROCK".to_vec(),
        root,
        ..Disc::default()
    }
}

fn names(fs: &mut Iso9660<Image>, dir: tairix_abi::driver::filesystem::NodeId) -> Vec<Vec<u8>> {
    listed(fs, dir, 0, &[])
        .expect("lists")
        .into_iter()
        .map(|(_, name)| name)
        .collect()
}

fn read_all(
    fs: &mut Iso9660<Image>,
    dir: tairix_abi::driver::filesystem::NodeId,
    name: &[u8],
) -> Vec<u8> {
    let node = fs.lookup(dir, name).expect("found");
    let size = fs.node_info(node).expect("info").size as usize;
    let mut out = vec![0u8; size + 7];
    let mut done = 0;
    while done < out.len() {
        // Odd-sized reads cross every block and part boundary.
        let end = (done + 1000).min(out.len());
        let n = fs
            .read_at(node, done as u64, &mut out[done..end])
            .expect("reads");
        if n == 0 {
            break;
        }
        done += n;
    }
    out.truncate(done);
    out
}

fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

#[test]
fn a_primary_tree_lists_names_without_versions_and_matches_them_blind_to_case() {
    let mut fs = mount(&disc(vec![
        Node::file(b"README.TXT;1", b"hello"),
        Node::file(b"MAKEFILE.;1", b"all:"),
        Node::dir(b"DOCS", vec![Node::file(b"GUIDE.MD;1", &pattern(5000, 3))]),
    ]));
    let root = fs.root();
    assert_eq!(fs.name_matching(), NameMatching::AsciiCaseInsensitive);
    assert_eq!(
        names(&mut fs, root),
        [&b"README.TXT"[..], b"MAKEFILE", b"DOCS"]
    );
    assert_eq!(read_all(&mut fs, root, b"readme.txt"), b"hello");
    let docs = fs.lookup(root, b"docs").expect("found");
    assert_eq!(fs.node_info(docs).expect("info").kind, NodeKind::Directory);
    assert_eq!(read_all(&mut fs, docs, b"GUIDE.MD"), pattern(5000, 3));
    let readme = fs.lookup(root, b"README.TXT").expect("found");
    let info = fs.node_info(readme).expect("info");
    assert_eq!(info.size, 5);
    assert_eq!(info.allocated, 2048);
    assert_eq!(info.nlink, 1);
    // 2021-06-15 12:30:00 UTC.
    assert_eq!(info.times.modified.secs(), 1_623_760_200);
    assert_eq!(fs.lookup(root, b"MISSING"), Err(DriverError::NotFound));
}

/// Every directory's `..` names its parent, so a climb from any depth of a
/// mastered disc reaches the root.
#[test]
fn a_climb_from_any_depth_reaches_the_root() {
    let mut fs = mount(&disc(vec![Node::dir(
        b"OUTER",
        vec![
            Node::file(b"SPACER.TXT;1", b"x"),
            Node::dir(b"INNER", vec![Node::dir(b"DEEPEST", vec![])]),
        ],
    )]));
    let root = fs.root();
    let outer = fs.lookup(root, b"OUTER").expect("found");
    let inner = fs.lookup(outer, b"INNER").expect("found");
    let deepest = fs.lookup(inner, b"DEEPEST").expect("found");
    assert_eq!(fs.parent(deepest), Ok(inner));
    assert_eq!(fs.parent(inner), Ok(outer));
    assert_eq!(fs.parent(outer), Ok(root));
    assert_eq!(fs.parent(root), Ok(root));
}

#[test]
fn a_high_sierra_disc_is_read_in_its_own_layout() {
    let mut file = Node::file(b"README.TXT;1", b"high sierra");
    // 1990-06-01 12:00:00, which High Sierra records with no offset from UTC.
    file.date = [90, 6, 1, 12, 0, 0, 0];
    let mut fs = mount(&Disc {
        volume_id: b"HSF".to_vec(),
        root: Node::dir(&[], vec![file, Node::dir(b"DOCS", vec![])]),
        standard: master::Standard::HighSierra,
        ..Disc::default()
    });
    let root = fs.root();
    assert_eq!(names(&mut fs, root), [&b"README.TXT"[..], b"DOCS"]);
    assert_eq!(read_all(&mut fs, root, b"README.TXT"), b"high sierra");
    let docs = fs.lookup(root, b"DOCS").expect("found");
    assert_eq!(
        fs.node_info(docs).expect("info").kind,
        NodeKind::Directory,
        "its flags byte sits a byte earlier"
    );
    let readme = fs.lookup(root, b"README.TXT").expect("found");
    assert_eq!(
        fs.node_info(readme).expect("info").times.modified.secs(),
        644_241_600
    );
}

#[test]
fn a_file_s_older_versions_and_associated_files_are_not_listed() {
    let mut fs = mount(&disc(vec![
        Node::file(b"NOTES.TXT;2", b"newest"),
        Node::file(b"NOTES.TXT;1", b"oldest"),
        Node::file(b"PICTURE.;1", b"resource fork").with_flags(ASSOCIATED),
        Node::file(b"PICTURE.;1", b"data fork"),
    ]));
    let root = fs.root();
    assert_eq!(names(&mut fs, root), [&b"NOTES.TXT"[..], b"PICTURE"]);
    assert_eq!(read_all(&mut fs, root, b"NOTES.TXT"), b"newest");
    assert_eq!(read_all(&mut fs, root, b"PICTURE"), b"data fork");
}

#[test]
fn a_resumed_listing_hands_each_entry_over_once() {
    let children: Vec<Node> = (0..150u8)
        .map(|i| Node::file(alloc::format!("FILE{i:03}.DAT;1").as_bytes(), &[i]))
        .collect();
    let mut fs = mount(&disc(children));
    let root = fs.root();
    let mut seen = Vec::new();
    let (mut cursor, mut after) = (0, Vec::new());
    while let Some((entry, name)) =
        tairix_abi::driver::filesystem::listing::first_listed(&mut fs, root, cursor, &after)
            .expect("lists")
    {
        seen.push(name.clone());
        cursor = entry.next_cursor;
        after = name;
    }
    assert_eq!(
        seen.len(),
        150,
        "every entry, the directory spanning sectors"
    );
    assert_eq!(seen, names(&mut fs, root));
}

#[test]
fn a_joliet_tree_is_mounted_with_unicode_names() {
    let mut joliet = Node::dir(
        &[],
        vec![
            Node::file(&master::ucs2("Résumé.txt;1"), b"cv"),
            Node::dir(&master::ucs2("Ünïcode"), vec![]),
        ],
    );
    joliet.date = [121, 6, 15, 12, 30, 0, 0];
    let mut fs = mount(&Disc {
        volume_id: b"PLAIN".to_vec(),
        root: Node::dir(&[], vec![Node::file(b"RESUME.TXT;1", b"cv")]),
        joliet: Some(joliet),
        ..Disc::default()
    });
    let root = fs.root();
    assert_eq!(
        names(&mut fs, root),
        ["Résumé.txt".as_bytes(), "Ünïcode".as_bytes()]
    );
    assert_eq!(read_all(&mut fs, root, "résumé.TXT".as_bytes()), b"cv");
}

#[test]
fn rock_ridge_names_owners_modes_links_and_times_are_read() {
    let name_use = [
        rr::nm(b"a long, mixed-Case name", true),
        rr::nm(b" continued.txt", false),
    ]
    .concat();
    let mut file = Node::file(b"LONGNAME.TXT;1", b"posix");
    file.system_use = [
        rr::px(0o100_640, 3, 1000, 100),
        name_use,
        rr::tf(
            0b0000_0110,
            &[[120, 1, 2, 3, 4, 5, 0], [119, 7, 8, 9, 10, 11, 0]],
        ),
    ]
    .concat();
    let link = Node::file(b"LINK.;1", &[]).with_use(
        [
            rr::px(0o120_777, 1, 0, 0),
            rr::nm(b"link", false),
            rr::sl(&[(8, b""), (0, b"usr"), (1, b"sh")], true),
            rr::sl(&[(0, b"are"), (4, b""), (0, b"x")], false),
        ]
        .concat(),
    );
    let device = Node::file(b"NULL.;1", &[])
        .with_use([rr::px(0o020_666, 1, 0, 0), rr::nm(b"null", false)].concat());
    let mut fs = mount(&rock_ridge(vec![file, link, device]));
    let root = fs.root();
    assert_eq!(fs.name_matching(), NameMatching::Exact);
    assert_eq!(
        names(&mut fs, root),
        [&b"a long, mixed-Case name continued.txt"[..], b"link"],
        "a device node is not listed"
    );
    let node = fs
        .lookup(root, b"a long, mixed-Case name continued.txt")
        .expect("found");
    let security = fs.security(node).expect("recorded");
    assert_eq!(
        (security.mode, security.uid, security.gid),
        (0o640, 1000, 100)
    );
    assert!(fs.records_ownership(node).expect("node"));
    let info = fs.node_info(node).expect("info");
    assert_eq!(info.nlink, 3);
    assert_eq!(
        info.times.modified.secs(),
        tairix_abi::time::days_from_civil(2020, 1, 2).expect("date") * 86_400
            + 3 * 3600
            + 4 * 60
            + 5
    );
    assert!(
        info.times.accessed.secs() < info.times.modified.secs(),
        "the access stamp is its own"
    );
    assert_eq!(
        fs.lookup(root, b"A LONG, MIXED-CASE NAME CONTINUED.TXT"),
        Err(DriverError::NotFound)
    );

    let link = fs.lookup(root, b"link").expect("found");
    let info = fs.node_info(link).expect("info");
    assert_eq!(info.kind, NodeKind::Symlink);
    let mut target = [0u8; 64];
    let len = fs.read_link(link, &mut target).expect("a link");
    assert_eq!(&target[..len], b"/usr/share/../x");
    assert_eq!(info.size, len as u64);
}

#[test]
fn a_system_use_continued_elsewhere_is_followed() {
    // The name's second part sits in a continuation area in block 19.
    let tail = rr::nm(b"-in-a-continuation", false);
    let mut file = Node::file(b"CE.;1", b"continued");
    file.system_use = [
        rr::px(0o100_644, 1, 0, 0),
        rr::nm(b"named", true),
        rr::ce(19, 100, tail.len() as u32),
    ]
    .concat();
    let mut mastered = master::master(&rock_ridge(vec![file]));
    mastered.image[19 * 2048 + 100..19 * 2048 + 100 + tail.len()].copy_from_slice(&tail);
    let mut fs = Iso9660::open(Image(mastered.image)).expect("mounts");
    let root = fs.root();
    assert_eq!(names(&mut fs, root), [&b"named-in-a-continuation"[..]]);
}

#[test]
fn a_continuation_that_loops_is_refused() {
    let mut file = Node::file(b"LOOP.;1", b"x");
    file.system_use = [rr::px(0o100_644, 1, 0, 0), rr::ce(19, 0, 28)].concat();
    let mut mastered = master::master(&rock_ridge(vec![file]));
    let looping = rr::ce(19, 0, 28);
    mastered.image[19 * 2048..19 * 2048 + looping.len()].copy_from_slice(&looping);
    let mut fs = Iso9660::open(Image(mastered.image)).expect("mounts");
    let root = fs.root();
    assert_eq!(
        listed(&mut fs, root, 0, &[]).map(|_| ()),
        Err(DriverError::DeviceFault)
    );
}

#[test]
fn a_relocated_directory_is_reached_where_it_belongs() {
    // The directory's records live under `RR_MOVED`, marked relocated; the
    // record in its real parent points at it.
    let moved = Node::dir(b"DEEP", vec![Node::file(b"LEAF.TXT;1", b"deep leaf")])
        .with_use([rr::re(), rr::nm(b"deep", false)].concat());
    let disc = rock_ridge(vec![
        Node::dir(b"RR_MOVED", vec![moved]).with_use(rr::nm(b"rr_moved", false))
    ]);
    let placed = master::master(&disc);
    // Depth-first: the root, `RR_MOVED`, then `DEEP`.
    let deep_extent = placed.directories[2];
    let pointer = Node {
        identifier: b"DEEP".to_vec(),
        system_use: [rr::cl(deep_extent), rr::nm(b"deep", false)].concat(),
        flags: 0,
        date: [121, 6, 15, 12, 30, 0, 0],
        kind: Kind::Pointer { extent: 0, len: 0 },
    };
    let disc = rock_ridge(vec![
        Node::dir(
            b"RR_MOVED",
            vec![
                Node::dir(b"DEEP", vec![Node::file(b"LEAF.TXT;1", b"deep leaf")])
                    .with_use([rr::re(), rr::nm(b"deep", false)].concat()),
            ],
        )
        .with_use(rr::nm(b"rr_moved", false)),
        Node::dir(b"A", vec![pointer]).with_use(rr::nm(b"a", false)),
    ]);
    let mut fs = Iso9660::open(Image(master::master(&disc).image)).expect("mounts");
    let root = fs.root();
    let moved_dir = fs.lookup(root, b"rr_moved").expect("found");
    assert!(
        names(&mut fs, moved_dir).is_empty(),
        "the relocated record is not listed"
    );
    let a = fs.lookup(root, b"a").expect("found");
    let deep = fs.lookup(a, b"deep").expect("found where it belongs");
    assert_eq!(fs.node_info(deep).expect("info").kind, NodeKind::Directory);
    assert_eq!(read_all(&mut fs, deep, b"LEAF.TXT"), b"deep leaf");
}

#[test]
fn risc_os_records_surface_as_the_acorn_attributes_adfs_presents() {
    let typed = 0xFFF0_0000 | (0xFEB << 8) | 0x12;
    let mut app = Node::dir(b"_BOOT", vec![Node::file(b"_RUN;1", b"*Run")]);
    app.system_use = archimedes(0, 0, 0x100 | 0x13);
    if let Kind::Dir { children, .. } = &mut app.kind {
        children[0].system_use = archimedes(typed, 0x3456_789A, 0x100 | 0x19);
    }
    let mut fs = mount(&disc(vec![app, Node::file(b"README;1", b"plain")]));
    let root = fs.root();
    assert_eq!(names(&mut fs, root), [&b"!BOOT"[..], b"README"]);
    let app = fs
        .lookup(root, b"!boot")
        .expect("found by its RISC OS name");
    let run = fs.lookup(app, b"!RUN").expect("found");
    let mut value = [0u8; 16];
    let len = fs
        .get_attr(run, b"acorn.filetype", &mut value)
        .expect("ok")
        .expect("typed");
    assert_eq!(&value[..len], b"feb");
    let len = fs
        .get_attr(run, b"acorn.datestamp", &mut value)
        .expect("ok")
        .expect("typed");
    assert_eq!(&value[..len], b"123456789a");
    let len = fs
        .get_attr(run, b"acorn.attr", &mut value)
        .expect("ok")
        .expect("present");
    assert_eq!(&value[..len], b"RL/r", "FileSwitch's R, L and public read");
    let len = fs
        .get_attr(app, b"acorn.attr", &mut value)
        .expect("ok")
        .expect("present");
    assert_eq!(&value[..len], b"RWD/r", "a directory carries the D bit");
    let keys: Vec<Vec<u8>> = (0..)
        .map_while(|index| {
            let mut key = [0u8; 32];
            fs.list_attr(run, index, &mut key)
                .expect("ok")
                .map(|len| key[..len].to_vec())
        })
        .collect();
    assert_eq!(keys.len(), 5);
    let plain = fs.lookup(root, b"README").expect("found");
    assert_eq!(fs.get_attr(plain, b"acorn.filetype", &mut value), Ok(None));
    assert_eq!(
        fs.set_attr(run, b"acorn.filetype", b"fff"),
        Err(DriverError::ReadOnly)
    );
}

fn attr(
    fs: &mut Iso9660<Image>,
    node: tairix_abi::driver::filesystem::NodeId,
    key: &[u8],
) -> Option<Vec<u8>> {
    let mut value = [0u8; 128];
    fs.get_attr(node, key, &mut value)
        .expect("ok")
        .map(|len| value[..len].to_vec())
}

fn attr_keys(
    fs: &mut Iso9660<Image>,
    node: tairix_abi::driver::filesystem::NodeId,
) -> Vec<Vec<u8>> {
    (0..)
        .map_while(|index| {
            let mut key = [0u8; 32];
            fs.list_attr(node, index, &mut key)
                .expect("ok")
                .map(|len| key[..len].to_vec())
        })
        .collect()
}

#[test]
fn apple_records_surface_as_the_mac_attributes() {
    let aaip = vec![b'A', b'A', 11, 1, 0, 1, 2, 3, 4, 5, 6];
    let mut fs = mount(&disc(vec![
        Node::file(b"HFS.TXT;1", b"h").with_use(apple::hfs(*b"TEXT", *b"ttxt", 0x0100)),
        Node::file(b"PRODOS.BIN;1", b"p").with_use(apple::prodos(0x06, 0x2000)),
        Node::file(b"ORIGINAL;1", b"o").with_use(apple::original_hfs(*b"APPL", *b"MSWD", 0x2000)),
        Node::file(b"XA.TXT;1", b"x")
            .with_use(apple::behind_xa(&apple::hfs(*b"PICT", *b"8BIM", 0))),
        Node::file(b"AAIP;1", b"a").with_use(aaip),
    ]));
    let root = fs.root();
    let hfs = fs.lookup(root, b"HFS.TXT").expect("found");
    assert_eq!(
        attr(&mut fs, hfs, b"mac.type").as_deref(),
        Some(&b"TEXT"[..])
    );
    assert_eq!(
        attr(&mut fs, hfs, b"mac.creator").as_deref(),
        Some(&b"ttxt"[..])
    );
    assert_eq!(
        attr(&mut fs, hfs, b"mac.finderflags").as_deref(),
        Some(&[1, 0][..])
    );
    assert_eq!(
        attr_keys(&mut fs, hfs),
        [&b"mac.type"[..], b"mac.creator", b"mac.finderflags"]
    );
    let prodos = fs.lookup(root, b"PRODOS.BIN").expect("found");
    assert_eq!(
        attr(&mut fs, prodos, b"mac.type").as_deref(),
        Some(&[b'p', 0x06, 0x20, 0x00][..])
    );
    assert_eq!(
        attr(&mut fs, prodos, b"mac.creator").as_deref(),
        Some(&b"pdos"[..])
    );
    assert_eq!(
        attr(&mut fs, prodos, b"mac.finderflags"),
        None,
        "ProDOS records no Finder flags"
    );
    assert_eq!(
        attr_keys(&mut fs, prodos),
        [&b"mac.type"[..], b"mac.creator"]
    );
    let original = fs.lookup(root, b"ORIGINAL").expect("found");
    assert_eq!(
        attr(&mut fs, original, b"mac.creator").as_deref(),
        Some(&b"MSWD"[..])
    );
    assert_eq!(
        attr(&mut fs, original, b"mac.finderflags").as_deref(),
        Some(&[0x20, 0][..])
    );
    let xa = fs.lookup(root, b"XA.TXT").expect("found");
    assert_eq!(
        attr(&mut fs, xa, b"mac.type").as_deref(),
        Some(&b"PICT"[..])
    );
    assert_eq!(
        attr_keys(&mut fs, xa),
        [&b"mac.type"[..], b"mac.creator"],
        "zero flags record none"
    );
    let aaip = fs.lookup(root, b"AAIP").expect("found");
    assert!(
        attr_keys(&mut fs, aaip).is_empty(),
        "libisofs' own AA entries are not Apple's"
    );
    assert_eq!(
        fs.set_attr(hfs, b"mac.type", b"BINA"),
        Err(DriverError::ReadOnly)
    );
}

#[test]
fn amiga_records_surface_as_the_amiga_attributes() {
    let continued = [
        rr::amiga(Some(0x0F), Some(b"Hello, "), true),
        rr::amiga(None, Some(b"world"), false),
    ]
    .concat();
    let stray = [
        rr::amiga(None, Some(b"kept"), false),
        rr::amiga(None, Some(b" stray"), false),
    ]
    .concat();
    let long = [
        rr::amiga(Some(0x40), Some(&[b'x'; 60]), true),
        rr::amiga(None, Some(&[b'y'; 30]), false),
    ]
    .concat();
    let mut fs = mount(&rock_ridge(vec![
        Node::file(b"CONT;1", b"c").with_use([rr::nm(b"continued", false), continued].concat()),
        Node::file(b"STRAY;1", b"s").with_use([rr::nm(b"stray", false), stray].concat()),
        Node::file(b"LONG;1", b"l").with_use([rr::nm(b"long", false), long].concat()),
        Node::file(b"MAC;1", b"m")
            .with_use([rr::nm(b"mac", false), apple::hfs(*b"TEXT", *b"R*ch", 0)].concat()),
    ]));
    let root = fs.root();
    let continued = fs.lookup(root, b"continued").expect("found");
    assert_eq!(
        attr(&mut fs, continued, b"amiga.protection").as_deref(),
        Some(&b"----rwed"[..])
    );
    assert_eq!(
        attr(&mut fs, continued, b"amiga.comment").as_deref(),
        Some(&b"Hello, world"[..])
    );
    assert_eq!(
        attr_keys(&mut fs, continued),
        [&b"amiga.protection"[..], b"amiga.comment"]
    );
    let stray = fs.lookup(root, b"stray").expect("found");
    assert_eq!(
        attr(&mut fs, stray, b"amiga.comment").as_deref(),
        Some(&b"kept"[..]),
        "an ended comment takes no more"
    );
    assert_eq!(attr(&mut fs, stray, b"amiga.protection"), None);
    let long = fs.lookup(root, b"long").expect("found");
    assert_eq!(
        attr(&mut fs, long, b"amiga.protection").as_deref(),
        Some(&b"-s------"[..])
    );
    assert_eq!(
        attr(&mut fs, long, b"amiga.comment"),
        None,
        "an over-long comment is dropped, not cut short"
    );
    let mac = fs.lookup(root, b"mac").expect("found");
    assert_eq!(
        attr(&mut fs, mac, b"mac.creator").as_deref(),
        Some(&b"R*ch"[..]),
        "Apple's entries sit among Rock Ridge's"
    );
}

#[test]
fn a_multi_extent_file_reads_across_its_parts() {
    let data = pattern(3 * 2048 + 700, 9);
    let mut file = Node::file(b"BIG.BIN;1", &data);
    if let Kind::File { parts, .. } = &mut file.kind {
        *parts = Some(vec![2048, 2048 + 300, 2048 + 400]);
    }
    let mut fs = mount(&disc(vec![file, Node::file(b"AFTER.TXT;1", b"next")]));
    let root = fs.root();
    assert_eq!(names(&mut fs, root), [&b"BIG.BIN"[..], b"AFTER.TXT"]);
    let big = fs.lookup(root, b"BIG.BIN").expect("found");
    let info = fs.node_info(big).expect("info");
    assert_eq!(info.size, data.len() as u64);
    assert_eq!(info.allocated, (1 + 2 + 2) * 2048);
    assert_eq!(read_all(&mut fs, root, b"BIG.BIN"), data);
}

#[test]
fn a_multi_extent_part_with_another_name_is_damage() {
    let mut file = Node::file(b"BIG.BIN;1", &pattern(4096, 1));
    if let Kind::File { parts, .. } = &mut file.kind {
        *parts = Some(vec![2048, 2048]);
    }
    let mut mastered = master::master(&disc(vec![file]));
    // Rename the second part.
    let root_dir = mastered.directories[0] as usize * 2048;
    let first = root_dir + 34 + 34;
    let second = first + usize::from(mastered.image[first]);
    mastered.image[second + 33] = b'X';
    let mut fs = Iso9660::open(Image(mastered.image)).expect("mounts");
    let root = fs.root();
    assert_eq!(
        listed(&mut fs, root, 0, &[]).map(|_| ()),
        Err(DriverError::DeviceFault)
    );
}

#[test]
fn an_interleaved_file_with_an_extended_attribute_record_reads_and_is_protected() {
    let data = pattern(5 * 2048 + 11, 4);
    let mut xar = vec![0u8; 2048];
    xar[..4].copy_from_slice(&[0xE8, 0x03, 0x03, 0xE8]); // owner 1000
    xar[4..8].copy_from_slice(&[0x64, 0x00, 0x00, 0x64]); // group 100
                                                          // Owner may read and execute, the group read, others nothing.
    let refused: u16 = 0xAAAA | (1 << 0) | (1 << 2) | (1 << 10) | (1 << 12) | (1 << 14);
    xar[8..10].copy_from_slice(&refused.to_be_bytes());
    xar[10..27].copy_from_slice(b"2001020304050600\0");
    let mut file = Node::file(b"MOVIE.MPG;1", &data).with_flags(PROTECTION);
    if let Kind::File {
        xar: x, interleave, ..
    } = &mut file.kind
    {
        *x = Some(xar);
        *interleave = Some((2, 1));
    }
    let mut fs = mount(&disc(vec![file]));
    let root = fs.root();
    assert_eq!(read_all(&mut fs, root, b"MOVIE.MPG"), data);
    let node = fs.lookup(root, b"MOVIE.MPG").expect("found");
    let security = fs.security(node).expect("recorded");
    assert_eq!(
        (security.mode, security.uid, security.gid),
        (0o540, 1000, 100)
    );
    let created = fs.node_info(node).expect("info").times.created;
    assert_eq!(
        created.secs(),
        tairix_abi::time::days_from_civil(2001, 2, 3).expect("date") * 86_400
            + 4 * 3600
            + 5 * 60
            + 6
    );
}

/// `data` compressed as a zisofs file of `log2` blocks.
fn zisofs(data: &[u8], log2: u8) -> Vec<u8> {
    let block = 1usize << log2;
    let blocks = data.len().div_ceil(block);
    let mut out = vec![0x37, 0xE4, 0x53, 0x96, 0xC9, 0xDB, 0xD6, 0x07];
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&[4, log2, 0, 0]);
    let table = out.len();
    out.resize(table + (blocks + 1) * 4, 0);
    let mut pointers = vec![out.len() as u32];
    for chunk in data.chunks(block) {
        if chunk.iter().all(|&b| b == 0) {
            // A block of zeros is stored as no bytes at all.
            pointers.push(out.len() as u32);
            continue;
        }
        let mut encoder = tairix_compress::zlib::Encoder::new();
        let mut compressed = vec![0u8; encoder.bound(chunk.len())];
        let n = encoder
            .compress(chunk, &mut compressed, tairix_compress::zlib::Flush::Finish)
            .expect("compresses");
        out.extend_from_slice(&compressed[..n]);
        pointers.push(out.len() as u32);
    }
    for (index, pointer) in pointers.iter().enumerate() {
        out[table + index * 4..table + index * 4 + 4].copy_from_slice(&pointer.to_le_bytes());
    }
    out
}

#[test]
fn a_zisofs_file_reads_as_its_uncompressed_bytes() {
    let mut data = pattern(100_000, 7);
    data[40_000..80_000].fill(0);
    let stored = zisofs(&data, 15);
    let file = Node::file(b"PACKED.BIN;1", &stored).with_use(
        [
            rr::px(0o100_644, 1, 0, 0),
            rr::nm(b"packed.bin", false),
            rr::zf(15, data.len() as u32),
        ]
        .concat(),
    );
    let mut fs = mount(&rock_ridge(vec![file]));
    let root = fs.root();
    let node = fs.lookup(root, b"packed.bin").expect("found");
    assert_eq!(fs.node_info(node).expect("info").size, data.len() as u64);
    assert_eq!(read_all(&mut fs, root, b"packed.bin"), data);
}

#[test]
fn a_zisofs_header_that_disagrees_with_its_record_is_damage() {
    let data = pattern(5000, 2);
    let mut stored = zisofs(&data, 15);
    stored[13] = 16; // the header's block size
    let file = Node::file(b"PACKED.BIN;1", &stored).with_use(
        [
            rr::px(0o100_644, 1, 0, 0),
            rr::nm(b"packed.bin", false),
            rr::zf(15, data.len() as u32),
        ]
        .concat(),
    );
    let mut fs = mount(&rock_ridge(vec![file]));
    let root = fs.root();
    let node = fs.lookup(root, b"packed.bin").expect("found");
    let mut buf = [0u8; 16];
    assert_eq!(fs.read_at(node, 0, &mut buf), Err(DriverError::DeviceFault));
}

#[test]
fn every_mutation_is_refused_as_read_only() {
    let mut fs = mount(&disc(vec![Node::file(b"A.TXT;1", b"a")]));
    let root = fs.root();
    assert_eq!(
        fs.create(
            root,
            b"new",
            NodeKind::RegularFile,
            &NodeSecurity::new(0o644, 0, 0)
        ),
        Err(DriverError::ReadOnly)
    );
    let node = fs.lookup(root, b"A.TXT").expect("found");
    assert_eq!(fs.write_at(node, 0, b"b"), Err(DriverError::ReadOnly));
    assert_eq!(fs.truncate(node, 0), Err(DriverError::ReadOnly));
    assert_eq!(fs.punch(node, 0, 1), Err(DriverError::ReadOnly));
    assert_eq!(fs.remove(root, b"A.TXT"), Err(DriverError::ReadOnly));
    assert_eq!(
        fs.rename(root, b"A.TXT", root, b"B.TXT"),
        Err(DriverError::ReadOnly)
    );
    assert_eq!(
        fs.create_link(root, b"l", b"A.TXT", &NodeSecurity::new(0o777, 0, 0)),
        Err(DriverError::ReadOnly)
    );
    assert_eq!(fs.flush(), Ok(()));
    assert!(!fs.records_ownership(node).expect("node"));
    assert_eq!(fs.security(node).expect("stand-in").mode, 0o644);
}

/// A finished disc keeps no record of a hold: a pin answers a live node's own
/// id, and every unpin answers released.
#[test]
fn a_pin_answers_the_node_it_holds() {
    let mut fs = mount(&disc(vec![
        Node::file(b"A.TXT;1", b"abc"),
        Node::dir(b"D", vec![]),
    ]));
    let root = fs.root();
    let file = fs.lookup(root, b"A.TXT").expect("found");
    let dir = fs.lookup(root, b"D").expect("found");
    for node in [root, file, dir, file] {
        assert_eq!(fs.pin(node), Ok(node));
    }
    let mut buf = [0u8; 8];
    assert_eq!(fs.read_at(file, 0, &mut buf), Ok(3));
    assert_eq!(fs.unpin(file), Ok(Unpinned::Released));
    assert_eq!(fs.unpin(file), Ok(Unpinned::Released));
    assert_eq!(fs.release_pins(), Ok(()));
    let dot = super::node_id(fs.directory(dir).expect("a directory").pos);
    assert_eq!(fs.pin(dot), Err(DriverError::NotFound), "a `.` record");
    assert_eq!(fs.pin(NodeId::NONE), Err(DriverError::NotFound));
    assert_eq!(fs.lookup(root, b"A.TXT"), Ok(file));
}

#[test]
fn reads_past_the_end_and_of_directories_are_refused_or_empty() {
    let mut fs = mount(&disc(vec![
        Node::file(b"A.TXT;1", b"abc"),
        Node::dir(b"D", vec![]),
    ]));
    let root = fs.root();
    let file = fs.lookup(root, b"A.TXT").expect("found");
    let mut buf = [0u8; 8];
    assert_eq!(fs.read_at(file, 3, &mut buf), Ok(0));
    assert_eq!(fs.read_at(file, 1, &mut buf), Ok(2));
    let dir = fs.lookup(root, b"D").expect("found");
    assert_eq!(fs.read_at(dir, 0, &mut buf), Err(DriverError::Unsupported));
    assert_eq!(fs.lookup(file, b"x"), Err(DriverError::Unsupported));
}

#[test]
fn the_volume_reports_its_size_and_no_free_space() {
    let mut fs = mount(&disc(vec![Node::file(b"A.TXT;1", &pattern(9000, 1))]));
    let stats = fs.stats().expect("stats");
    assert_eq!(stats.block_size, 2048);
    assert!(stats.total_blocks > 20);
    assert_eq!((stats.free_blocks, stats.avail_blocks), (0, 0));
}

#[test]
fn a_record_that_crosses_its_sector_is_damage() {
    let files = (0..60)
        .map(|i| Node::file(alloc::format!("F{i:02}.TXT;1").as_bytes(), b"f"))
        .collect();
    let mut mastered = master::master(&disc(files));
    let root_dir = mastered.directories[0] as usize * 2048;
    // The records fill the directory's first sector, then its padding; a
    // record starting in the padding runs past the sector's end.
    let mut at = root_dir;
    while mastered.image[at] != 0 {
        at += usize::from(mastered.image[at]);
    }
    let padding = root_dir + 2048 - at;
    assert!((1..34).contains(&padding), "{padding} bytes of padding");
    mastered.image[at] = (padding + 1) as u8;
    let mut fs = Iso9660::open(Image(mastered.image)).expect("mounts");
    let root = fs.root();
    assert_eq!(
        listed(&mut fs, root, 0, &[]).map(|list| list.len()),
        Err(DriverError::DeviceFault)
    );
    assert_eq!(fs.lookup(root, b"F59.TXT"), Err(DriverError::DeviceFault));
}

#[test]
fn a_record_that_does_not_decode_is_damage_not_absence() {
    let mut mastered = master::master(&disc(vec![
        Node::file(b"A.TXT;1", b"a"),
        Node::file(b"B.TXT;1", b"b"),
    ]));
    let root_dir = mastered.directories[0] as usize * 2048;
    // `A`'s record, after `.` and `..`, claims a name of no bytes.
    mastered.image[root_dir + 68 + 32] = 0;
    let mut fs = Iso9660::open(Image(mastered.image)).expect("mounts");
    let root = fs.root();
    assert_eq!(fs.lookup(root, b"B.TXT"), Err(DriverError::DeviceFault));
}

#[test]
fn hostile_rock_ridge_names_list_as_names_no_path_can_traverse() {
    let names_use = |name: &[u8]| [rr::px(0o100_644, 1, 0, 0), rr::nm(name, false)].concat();
    let profile = Node::file(b"PROFILE.;1", b"stolen").with_use(names_use(b"../../u/.profile"));
    let dots = Node::file(b"DOTS.;1", b"dots").with_use(names_use(b".."));
    let empty = Node::file(b"EMPTY.;1", b"empty").with_use(names_use(b""));
    let latin = Node::file(b"LATIN.;1", b"latin").with_use(names_use(&[b'r', 0xE9, b's']));
    let mut fs = mount(&rock_ridge(vec![profile, dots, empty, latin]));
    let root = fs.root();
    let listed = names(&mut fs, root);
    assert_eq!(listed.len(), 4);
    for name in &listed {
        let text = core::str::from_utf8(name).expect("UTF-8");
        assert!(
            !text.contains('/') && text != "." && text != ".." && !text.is_empty(),
            "{text}"
        );
        let node = fs
            .lookup(root, name)
            .expect("found by the name it lists under");
        assert_eq!(
            fs.node_info(node).expect("info").kind,
            NodeKind::RegularFile
        );
    }
    let profile = listed
        .iter()
        .find(|name| name.starts_with(b".._.._u_.profile~"))
        .expect("aliased");
    assert_eq!(read_all(&mut fs, root, profile), b"stolen");
}

#[test]
fn a_multi_extent_file_reaching_into_a_third_sector_is_read() {
    // Each part's record carries a long Rock Ridge name, so its parts' records
    // run on through more than two sectors.
    let name = [b'n'; 120];
    let use_of = || [rr::px(0o100_644, 1, 0, 0), rr::nm(&name, false)].concat();
    let mut file = Node::file(b"MULTI.;1", &pattern(30 * 2048, 5)).with_use(use_of());
    if let Kind::File { parts, .. } = &mut file.kind {
        *parts = Some(vec![2048; 30]);
    }
    let mut fs = mount(&rock_ridge(vec![file]));
    let root = fs.root();
    let node = fs.lookup(root, &name).expect("found");
    assert_eq!(fs.node_info(node).expect("info").size, 30 * 2048);
    assert_eq!(read_all(&mut fs, root, &name), pattern(30 * 2048, 5));
}

#[test]
fn a_rock_ridge_sparse_file_lists_its_size_and_refuses_its_bytes() {
    let use_of =
        |name: &[u8], sf: Vec<u8>| [rr::px(0o100_644, 1, 0, 0), rr::nm(name, false), sf].concat();
    let sparse = Node::file(b"SPARSE.;1", b"table").with_use(use_of(b"sparse", rr::sf(5 << 30)));
    let mut broken = rr::sf(1);
    // The high half's big-endian copy disagrees with its little-endian one.
    broken[8] ^= 1;
    let broken = Node::file(b"BROKEN.;1", b"x").with_use(use_of(b"broken", broken));
    let mut fs = mount(&rock_ridge(vec![sparse, broken]));
    let root = fs.root();
    assert_eq!(
        names(&mut fs, root),
        [&b"sparse"[..]],
        "a malformed sparse file is not served"
    );
    let node = fs.lookup(root, b"sparse").expect("found");
    assert_eq!(fs.node_info(node).expect("info").size, 5 << 30);
    let mut buf = [0u8; 4];
    assert_eq!(fs.read_at(node, 0, &mut buf), Err(DriverError::Unsupported));
    assert_eq!(
        fs.seek_region(node, 0, tairix_abi::fs::SeekRegion::Data),
        Err(DriverError::Unsupported),
        "no region is claimed of bytes that are refused"
    );
}

#[test]
fn a_root_recorded_at_the_volumes_first_byte_is_not_the_reserved_none() {
    let mut mastered = master::master(&disc(vec![Node::file(b"A.TXT;1", b"a")]));
    let root_dir = mastered.directories[0] as usize * 2048;
    // The system area, which a disc's author controls, holds a copy of the
    // root's records, and the primary descriptor names it as the root.
    let records = mastered.image[root_dir..root_dir + 2048].to_vec();
    mastered.image[..2048].copy_from_slice(&records);
    let root_record = 16 * 2048 + 156;
    mastered.image[root_record + 2..root_record + 10].copy_from_slice(&[0; 8]);
    let mut fs = Iso9660::open(Image(mastered.image)).expect("mounts");
    let root = fs.root();
    assert_ne!(root, tairix_abi::driver::filesystem::NodeId::NONE);
    assert_eq!(names(&mut fs, root), [&b"A.TXT"[..]]);
    assert_eq!(
        fs.node_info(tairix_abi::driver::filesystem::NodeId::NONE),
        Err(DriverError::NotFound)
    );
}

#[test]
fn a_device_without_a_volume_is_refused() {
    assert_eq!(
        Iso9660::open(Image(vec![0u8; 64 * 1024])).map(|_| ()).err(),
        Some(DriverError::BadMagic)
    );
}

/// A device over an image, counting the reads it serves.
struct Counted {
    image: Image,
    reads: usize,
}

impl Block for Counted {
    fn geometry(&self) -> Result<BlockGeometry, DriverError> {
        self.image.geometry()
    }

    fn read_blocks(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        self.reads += 1;
        self.image.read_blocks(lba, buf)
    }

    fn write_blocks(&mut self, lba: u64, buf: &[u8]) -> Result<(), DriverError> {
        self.image.write_blocks(lba, buf)
    }

    fn flush(&mut self) -> Result<(), DriverError> {
        Ok(())
    }
}

fn counted(image: Vec<u8>) -> Iso9660<Counted> {
    Iso9660::open(Counted {
        image: Image(image),
        reads: 0,
    })
    .expect("mounts")
}

#[test]
fn continuation_areas_packed_into_one_sector_are_read_once() {
    let tails: Vec<Vec<u8>> = (0..20)
        .map(|i| rr::nm(alloc::format!("name-{i:02}").as_bytes(), false))
        .collect();
    let files = |block: u32| -> Vec<Node> {
        let mut at = 0u32;
        tails
            .iter()
            .enumerate()
            .map(|(i, tail)| {
                let use_ = [
                    rr::px(0o100_644, 1, 0, 0),
                    rr::ce(block, at, tail.len() as u32),
                ]
                .concat();
                at += tail.len() as u32;
                Node::file(alloc::format!("F{i:02}.;1").as_bytes(), b"x").with_use(use_)
            })
            .collect()
    };
    // The areas live in a block past everything else mastered.
    let block = (master::master(&rock_ridge(files(0))).image.len() / 2048) as u32;
    let mut image = master::master(&rock_ridge(files(block))).image;
    image.resize(image.len() + 2048, 0);
    let mut at = block as usize * 2048;
    for tail in &tails {
        image[at..at + tail.len()].copy_from_slice(tail);
        at += tail.len();
    }
    let mut fs = counted(image);
    let root = fs.root();
    fs.dev.device_mut().reads = 0;
    let listing = listed(&mut fs, root, 0, &[]).expect("lists");
    assert_eq!(listing.len(), 20);
    assert_eq!(listing[3].1, b"name-03");
    let reads = fs.dev.device().reads;
    assert!(
        reads < 8,
        "{reads} reads for one directory and one sector of areas"
    );
}

#[test]
fn a_lookup_resolves_only_the_name_it_finds() {
    let files = (0..10)
        .map(|i| {
            let mut file = Node::file(alloc::format!("F{i:02}.TXT;1").as_bytes(), b"x");
            if let Kind::File { xar, .. } = &mut file.kind {
                *xar = Some(vec![0u8; 250]);
            }
            file
        })
        .collect();
    let mut fs = counted(master::master(&disc(files)).image);
    let root = fs.root();
    fs.dev.device_mut().reads = 0;
    fs.lookup(root, b"F09.TXT").expect("found");
    let reads = fs.dev.device().reads;
    assert!(
        reads < 6,
        "{reads} reads: every passed name's attribute record was read"
    );
}

#[test]
fn relocated_directories_leave_the_listing_sector_held() {
    let deep = || {
        Node::dir(b"DEEP", vec![Node::file(b"LEAF.TXT;1", b"deep leaf")])
            .with_use([rr::re(), rr::nm(b"deep", false)].concat())
    };
    let moved = || Node::dir(b"RR_MOVED", vec![deep()]).with_use(rr::nm(b"rr_moved", false));
    let deep_extent = master::master(&rock_ridge(vec![moved()])).directories[2];
    let pointers = (0..6)
        .map(|i| Node {
            identifier: alloc::format!("P{i}").into_bytes(),
            system_use: [
                rr::cl(deep_extent),
                rr::nm(alloc::format!("p{i}").as_bytes(), false),
            ]
            .concat(),
            flags: 0,
            date: [121, 6, 15, 12, 30, 0, 0],
            kind: Kind::Pointer { extent: 0, len: 0 },
        })
        .collect();
    let disc = rock_ridge(vec![
        moved(),
        Node::dir(b"A", pointers).with_use(rr::nm(b"a", false)),
    ]);
    let mut fs = counted(master::master(&disc).image);
    let root = fs.root();
    let a = fs.lookup(root, b"a").expect("found");
    fs.dev.device_mut().reads = 0;
    let listing = listed(&mut fs, a, 0, &[]).expect("lists");
    assert_eq!(listing.len(), 6);
    let reads = fs.dev.device().reads;
    assert!(
        reads <= 6 + 3,
        "{reads} reads for six relocated directories"
    );
}

#[test]
fn the_served_driver_offers_every_disc_read_only() {
    use tairix_fsserve::{ServedFormat, WritebackBudget};
    let budget = WritebackBudget {
        backing: 1 << 30,
        gauge: &tairix_reclaim::Unpressured,
    };
    let image = || Image(master::master(&disc(Vec::new())).image);
    let mut asked_to_write = crate::Served::open(image(), false, budget).expect("mounts");
    assert_eq!(
        asked_to_write.format,
        tairix_abi::volume::FormatName::Iso9660
    );
    assert_eq!(
        asked_to_write.offer().write_refusal,
        Some(tairix_abi::driver::filesystem::WriteRefusal::FormatReadOnly)
    );
    let read_only = crate::Served::open(image(), true, budget).expect("mounts");
    assert_eq!(read_only.write_refusal, None);
}
