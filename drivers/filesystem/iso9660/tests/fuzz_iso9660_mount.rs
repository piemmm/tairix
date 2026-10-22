//! Deterministic fuzz harness for the ISO 9660 driver (a parser of untrusted
//! disc bytes).
//!
//! A disc is outside TAIRiX's trust boundary: any byte of its descriptors,
//! directory records, system use, continuation areas, extended attribute
//! records and zisofs streams may be hostile. Whatever it holds, mounting it
//! and walking every tree must answer values or errors — never panic, read
//! out of bounds, or fail to end. The run aborting *is* the failure.
//!
//! A per-run-seeded `Prng` mutates mastered discs carrying every extension
//! and the `genisoimage` fixtures, biased towards the bytes that carry
//! structure. A plain `cargo test` runs the fixed [`SMOKE_ITERATIONS`]
//! sweep; `cargo xtask fuzz` extends it to a wall-clock budget.

use tairix_test_iso9660_image as master;
#[path = "../../ext4/tests/support/sparse.rs"]
mod sparse;

use master::{apple, archimedes, rr, Disc, Kind, Node, PROTECTION};
use tairix_abi::driver::block::{Block, BlockGeometry};
use tairix_abi::driver::filesystem::{
    DirVisit, FilesystemAttrs, FilesystemRead, FilesystemSecurity, FilesystemStats, NodeId,
    NodeKind,
};
use tairix_abi::DriverError;
use tairix_drv_fs_iso9660::Iso9660;
use tairix_fuzzseed::Prng;

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_ITERATIONS: u64 = 1_500;

/// How deep a walk descends, and how many nodes it visits.
const MAX_DEPTH: usize = 12;
const MAX_NODES: usize = 400;

struct Image(Vec<u8>);

impl Block for Image {
    fn geometry(&self) -> Result<BlockGeometry, DriverError> {
        Ok(BlockGeometry {
            block_size: 512,
            block_count: (self.0.len() / 512) as u64,
        })
    }

    fn read_blocks(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        let at = usize::try_from(lba).map_err(|_| DriverError::OutOfRange)? * 512;
        let src = self
            .0
            .get(at..at.checked_add(buf.len()).ok_or(DriverError::OutOfRange)?)
            .ok_or(DriverError::OutOfRange)?;
        buf.copy_from_slice(src);
        Ok(())
    }

    fn write_blocks(&mut self, _: u64, _: &[u8]) -> Result<(), DriverError> {
        Err(DriverError::ReadOnly)
    }

    fn flush(&mut self) -> Result<(), DriverError> {
        Ok(())
    }
}

/// A disc using every extension the driver reads.
fn everything() -> Vec<u8> {
    let mut root = Node::dir(&[], Vec::new());
    let mut xar = vec![0u8; 2048];
    xar[..8].copy_from_slice(&[1, 0, 0, 1, 2, 0, 0, 2]);
    let mut interleaved = Node::file(b"MOVIE.MPG;1", &[7u8; 9000]).with_flags(PROTECTION);
    if let Kind::File {
        xar: x, interleave, ..
    } = &mut interleaved.kind
    {
        *x = Some(xar);
        *interleave = Some((2, 1));
    }
    let mut multi = Node::file(b"BIG.BIN;1", &[9u8; 6000]);
    if let Kind::File { parts, .. } = &mut multi.kind {
        *parts = Some(vec![2048, 3000, 952]);
    }
    let zipped = {
        let mut out = vec![0x37, 0xE4, 0x53, 0x96, 0xC9, 0xDB, 0xD6, 0x07];
        out.extend_from_slice(&40_000u32.to_le_bytes());
        out.extend_from_slice(&[4, 15, 0, 0]);
        out.extend_from_slice(&28u32.to_le_bytes());
        out.extend_from_slice(&28u32.to_le_bytes());
        out.extend_from_slice(&28u32.to_le_bytes());
        out
    };
    let children = vec![
        Node::file(b"README.TXT;1", b"hello").with_use(
            [
                rr::px(0o100_644, 1, 0, 0),
                rr::nm(b"readme.txt", false),
                rr::tf(0x0E, &[[121, 1, 1, 0, 0, 0, 0]; 3]),
            ]
            .concat(),
        ),
        Node::file(b"LINK.;1", &[]).with_use(
            [
                rr::px(0o120_777, 1, 0, 0),
                rr::sl(&[(8, b""), (0, b"a"), (1, b"b")], false),
            ]
            .concat(),
        ),
        Node::file(b"ZIPPED.;1", &zipped)
            .with_use([rr::px(0o100_644, 1, 0, 0), rr::zf(15, 40_000)].concat()),
        Node::dir(
            b"_APP",
            vec![Node::file(b"_RUN;1", b"x").with_use(archimedes(0xFFFF_EB00, 0, 0x113))],
        )
        .with_use(archimedes(0, 0, 0x100)),
        Node::file(b"AMIGA.;1", b"a").with_use(
            [
                rr::amiga(Some(0x0F), Some(b"one "), true),
                rr::amiga(None, Some(b"two"), false),
                apple::hfs(*b"TEXT", *b"ttxt", 1),
            ]
            .concat(),
        ),
        interleaved,
        multi,
    ];
    if let Kind::Dir {
        children: c,
        dot_use,
    } = &mut root.kind
    {
        *c = children;
        *dot_use = [rr::root(0), rr::px(0o040_755, 2, 0, 0)].concat();
    }
    master::master(&Disc {
        volume_id: b"FUZZ".to_vec(),
        root,
        joliet: Some(Node::dir(
            &[],
            vec![Node::file(&master::ucs2("Joliet name.txt;1"), b"j")],
        )),
        ..Disc::default()
    })
    .image
}

/// A disc without SUSP whose records carry Apple's information.
fn finder() -> Vec<u8> {
    master::master(&Disc {
        volume_id: b"APPLE".to_vec(),
        root: Node::dir(
            &[],
            vec![
                Node::file(b"HFS.;1", b"h").with_use(apple::hfs(*b"APPL", *b"MSWD", 0x2000)),
                Node::file(b"PRODOS.;1", b"p").with_use(apple::prodos(6, 0x2000)),
                Node::file(b"ORIGINAL.;1", b"o")
                    .with_use(apple::original_hfs(*b"PICT", *b"8BIM", 0)),
                Node::file(b"XA.;1", b"x")
                    .with_use(apple::behind_xa(&apple::hfs(*b"TEXT", *b"R*ch", 0))),
            ],
        ),
        ..Disc::default()
    })
    .image
}

/// Mount `bytes` and walk everything it holds. Must never panic.
fn exercise(bytes: Vec<u8>) {
    let Ok(mut fs) = Iso9660::open(Image(bytes)) else {
        return;
    };
    let _ = fs.stats();
    let mut visited = 0;
    let root = fs.root();
    visit(&mut fs, root, 0, &mut visited);
}

fn visit(fs: &mut Iso9660<Image>, dir: NodeId, depth: usize, visited: &mut usize) {
    if depth > MAX_DEPTH {
        return;
    }
    let mut children = Vec::new();
    let _ = fs.read_dir(dir, 0, &[], &mut |entry, name| {
        children.push((entry.node, entry.info.kind, name.to_vec()));
        if children.len() >= MAX_NODES {
            DirVisit::Stop
        } else {
            DirVisit::Take
        }
    });
    for (node, kind, name) in children {
        *visited += 1;
        if *visited > MAX_NODES {
            return;
        }
        let _ = fs.lookup(dir, &name);
        let _ = fs.node_info(node);
        let _ = fs.security(node);
        let mut value = [0u8; 64];
        for index in 0..12 {
            if let Ok(Some(len)) = fs.list_attr(node, index, &mut value) {
                let key = value[..len].to_vec();
                let _ = fs.get_attr(node, &key, &mut value);
            }
        }
        match kind {
            NodeKind::Directory => visit(fs, node, depth + 1, visited),
            NodeKind::RegularFile => {
                let mut buf = [0u8; 4096];
                let _ = fs.read_at(node, 0, &mut buf);
                let _ = fs.read_at(node, 70_000, &mut buf);
            }
            NodeKind::Symlink => {
                let _ = fs.read_link(node, &mut value);
            }
        }
    }
}

#[test]
fn mounting_any_disc_never_panics() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let seeds: Vec<(Vec<u8>, Vec<usize>)> = [
        everything(),
        finder(),
        sparse::sparse(include_bytes!("fixtures/rr-joliet.sparse")),
        sparse::sparse(include_bytes!("fixtures/joliet.sparse")),
        sparse::sparse(include_bytes!("fixtures/bridge.sparse")),
    ]
    .into_iter()
    .map(|image| {
        let hot = image
            .iter()
            .enumerate()
            .filter(|(_, &b)| b != 0)
            .map(|(at, _)| at)
            .collect();
        (image, hot)
    })
    .collect();
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "mounting_any_disc_never_panics",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut iteration: u64 = 0;
    loop {
        let (seed, hot) = rng.pick(&seeds);
        let mut bytes = seed.clone();
        for _ in 0..rng.at_most(32) {
            let at = if rng.below(5) == 0 {
                rng.below(bytes.len())
            } else {
                *rng.pick(hot)
            };
            bytes[at] = match rng.below(3) {
                0 => rng.next_u8(),
                1 => bytes[at] ^ (1 << rng.below(8)),
                _ => rng.edge_biased_u8(),
            };
        }
        if rng.below(8) == 0 {
            let keep = rng.at_most(bytes.len()).max(512) / 512 * 512;
            bytes.truncate(keep);
        }
        exercise(bytes);
        iteration += 1;
        if !tairix_fuzzseed::within_budget(deadline) && iteration >= SMOKE_ITERATIONS {
            break;
        }
    }
}
