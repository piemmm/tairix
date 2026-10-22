//! Deterministic fuzz harness for the UDF driver (a parser and writer of
//! untrusted volume bytes).
//!
//! A stick or disc is outside TAIRiX's trust boundary: any byte of its
//! anchors, descriptor sequences, partition maps, sparing and allocation
//! tables, file entries, allocation descriptors and identifiers may be
//! hostile. Whatever the volume holds, mounting it, walking it, reading it
//! and writing to it must answer values or errors — never panic, read or
//! write out of bounds, or fail to end. The run aborting *is* the failure.
//!
//! A per-run-seeded `Prng` mutates volumes the driver's own formatter laid
//! out and populated, and the `genisoimage` bridge disc, biased towards the
//! bytes that carry structure. A plain `cargo test` runs the fixed
//! [`SMOKE_ITERATIONS`] sweep; `cargo xtask fuzz` extends it to a
//! wall-clock budget.

#[path = "../../ext4/tests/support/sparse.rs"]
mod sparse;

use tairix_abi::driver::block::{Block, BlockGeometry};
use tairix_abi::driver::filesystem::{
    DirVisit, FilesystemAttrs, FilesystemRead, FilesystemSecurity, FilesystemStats,
    FilesystemWrite, NodeId, NodeKind,
};
use tairix_abi::fs::SeekRegion;
use tairix_abi::time::Time64;
use tairix_abi::DriverError;
use tairix_drv_fs_udf::{FormatOptions, Udf};
use tairix_fuzzseed::Prng;

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_ITERATIONS: u64 = 600;

const MAX_DEPTH: usize = 8;
const MAX_NODES: usize = 200;

struct Disk {
    block_size: u32,
    bytes: Vec<u8>,
}

impl Block for Disk {
    fn geometry(&self) -> Result<BlockGeometry, DriverError> {
        Ok(BlockGeometry {
            block_size: self.block_size,
            block_count: (self.bytes.len() / self.block_size as usize) as u64,
        })
    }

    fn read_blocks(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        let at =
            usize::try_from(lba).map_err(|_| DriverError::OutOfRange)? * self.block_size as usize;
        let end = at.checked_add(buf.len()).ok_or(DriverError::OutOfRange)?;
        buf.copy_from_slice(self.bytes.get(at..end).ok_or(DriverError::OutOfRange)?);
        Ok(())
    }

    fn write_blocks(&mut self, lba: u64, buf: &[u8]) -> Result<(), DriverError> {
        let at =
            usize::try_from(lba).map_err(|_| DriverError::OutOfRange)? * self.block_size as usize;
        let end = at.checked_add(buf.len()).ok_or(DriverError::OutOfRange)?;
        self.bytes
            .get_mut(at..end)
            .ok_or(DriverError::OutOfRange)?
            .copy_from_slice(buf);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), DriverError> {
        Ok(())
    }
}

/// A formatted volume of `block_size` blocks, populated: directories, a
/// fragmented file spilling into continuation blocks, a hole, links.
fn populated(block_size: u32, revision: u16) -> Vec<u8> {
    let disk = Disk {
        block_size,
        bytes: vec![0u8; 2 << 20],
    };
    let options = FormatOptions {
        label: "Fuzz",
        revision,
        serial: [9; 8],
        now: Time64::from_secs(1_700_000_000),
    };
    let mut fs = Udf::format(disk, &options).expect("formats");
    let root = fs.root();
    let dir = fs.create(root, b"dir", NodeKind::Directory).expect("mkdir");
    fs.create(dir, b"inner.txt", NodeKind::RegularFile)
        .expect("create");
    fs.write_all(dir, b"inner.txt", 0, b"inner").expect("write");
    fs.create(root, b"a", NodeKind::RegularFile)
        .expect("create");
    fs.create(root, b"b", NodeKind::RegularFile)
        .expect("create");
    for i in 0..60u8 {
        let block = vec![i; block_size as usize];
        fs.write_all(root, b"a", u64::from(i) * u64::from(block_size), &block)
            .expect("a");
        fs.write_all(root, b"b", u64::from(i) * u64::from(block_size), &block)
            .expect("b");
    }
    fs.punch(
        root,
        b"a",
        u64::from(block_size) * 3,
        u64::from(block_size) * 4,
    )
    .expect("punch");
    let a = fs.lookup(root, b"a").expect("a");
    fs.link(dir, b"a-again", a).expect("link");
    fs.create_link(root, b"link", b"/dir/../a")
        .expect("symlink");
    fs.set_attr(a, b"mac.type", b"TEXT").expect("type");
    fs.set_attr(dir, b"mac.finderflags", &[0x40, 0])
        .expect("flags");
    fs.flush().expect("flush");
    fs.into_device().bytes
}

fn exercise(bytes: Vec<u8>, block_size: u32, write: bool) {
    let Ok(mut fs) = Udf::open(Disk { block_size, bytes }) else {
        return;
    };
    let _ = fs.stats();
    let root = fs.root();
    let mut visited = 0;
    visit(&mut fs, root, 0, &mut visited);
    if write && fs.write_refusal().is_none() {
        let _ = fs.create(root, b"new", NodeKind::RegularFile);
        let _ = fs.write_all(root, b"new", 0, &[5u8; 3000]);
        let _ = fs.write_all(root, b"a", 100, &[6u8; 5000]);
        if let Ok(a) = fs.lookup(root, b"a") {
            let _ = fs.set_attr(a, b"mac.creator", b"ttxt");
            let _ = fs.remove_attr(a, b"mac.type");
        }
        let _ = fs.truncate(root, b"b", 10);
        let _ = fs.rename(root, b"dir", root, b"moved");
        let _ = fs.remove(root, b"a");
        let _ = fs.flush();
        visited = 0;
        visit(&mut fs, root, 0, &mut visited);
    }
}

fn visit(fs: &mut Udf<Disk>, dir: NodeId, depth: usize, visited: &mut usize) {
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
        let mut key = [0u8; 32];
        for index in 0..4 {
            if let Ok(Some(len)) = fs.list_attr(node, index, &mut key) {
                let mut value = [0u8; 16];
                let _ = fs.get_attr(node, &key[..len], &mut value);
            }
        }
        match kind {
            NodeKind::Directory => visit(fs, node, depth + 1, visited),
            NodeKind::RegularFile => {
                let mut buf = [0u8; 4096];
                let _ = fs.read_at(node, 0, &mut buf);
                let _ = fs.read_at(node, 100_000, &mut buf);
                let _ = fs.seek_region(node, 0, SeekRegion::Hole);
            }
            NodeKind::Symlink => {
                let mut target = [0u8; 256];
                let _ = fs.read_link(node, &mut target);
            }
        }
    }
}

#[test]
fn mounting_any_volume_never_panics() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let seeds: Vec<(Vec<u8>, u32, Vec<usize>)> = [
        (populated(512, 0x0201), 512),
        (populated(2048, 0x0150), 2048),
        (
            sparse::sparse(include_bytes!("../../iso9660/tests/fixtures/bridge.sparse")),
            2048,
        ),
    ]
    .into_iter()
    .map(|(image, block_size)| {
        let hot = image
            .iter()
            .enumerate()
            .filter(|(_, &b)| b != 0)
            .map(|(at, _)| at)
            .collect();
        (image, block_size, hot)
    })
    .collect();
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "mounting_any_volume_never_panics",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut iteration: u64 = 0;
    loop {
        let (seed, block_size, hot) = rng.pick(&seeds);
        let mut bytes = seed.clone();
        for _ in 0..rng.at_most(24) {
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
        exercise(bytes, *block_size, rng.below(2) == 0);
        iteration += 1;
        if !tairix_fuzzseed::within_budget(deadline) && iteration >= SMOKE_ITERATIONS {
            break;
        }
    }
}
