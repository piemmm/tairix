//! Deterministic fuzz harness for the archive driver's open, walk and
//! commit paths.
//!
//! An archive is a stranger's bytes, parsed only by this driver. The single
//! invariant: [`Served::open`] never panics for any archive — it serves one
//! it can believe and refuses the rest — and an archive that opens is walked,
//! has every file read through, and, where it may be written, takes a change
//! committed and opened again, all without panicking.
//!
//! Each writable format's base is written by the driver itself, from the
//! least archive of its format, so the bases carry what the writer writes:
//! directories, compressed and stored members, RISC OS fields, a DFS disc's
//! files on each side of it. RAR, which is only read, takes its bases from
//! the test fixtures: every version's compression, solid runs, PPMd and the
//! filters. A seeded stream then
//! flips, cuts and overwrites their bytes; a plain `cargo test` runs a quick
//! smoke sample, and the soak (`TAIRIX_FUZZ_BUDGET_SECS`) gives each base an
//! equal share of the budget.

use std::rc::Rc;

use tairix_abi::driver::filesystem::{
    DirVisit, FilesystemAttrs, FilesystemRead, FilesystemWrite, NodeId, NodeKind, NodeSecurity,
};
use tairix_drv_fs_archive::{ArchiveFs, Served};
use tairix_fsserve::{
    GrantedVolume, MemoryAccess, MemoryAttachment, ServedArchive, WritebackBudget,
};
use tairix_fuzzseed::Prng;
use tairix_reclaim::Unpressured;
use tairix_test_archive_image::Format;

/// PRNG mutations per base for the quick smoke pass (no budget set).
const SMOKE_ITERATIONS: u64 = 64;

/// The most bytes a walk reads of one file, bounding what a stranger's
/// lengths make the harness do.
const READ_MAX: u64 = 1 << 20;

const RECORD: NodeSecurity = NodeSecurity::new(0o644, 0, 0);

/// An archive the driver opened, and whether it may be written.
struct Open {
    attachment: Rc<MemoryAttachment>,
    fs: ArchiveFs<MemoryAccess>,
    writable: bool,
}

fn open(archive: &[u8], writable: bool, name: &[u8]) -> Option<Open> {
    let attachment = MemoryAttachment::new(archive, writable, name);
    let granted = GrantedVolume::open(attachment.access()).ok()?;
    let read_only = granted.info().read_only;
    let budget = WritebackBudget {
        backing: 1 << 30,
        gauge: &Unpressured,
    };
    let opened = Served::open(granted, read_only, budget, &tairix_log::DiscardSink).ok()?;
    Some(Open {
        attachment,
        fs: opened.fs,
        writable: !read_only && opened.write_refusal.is_none(),
    })
}

/// Walk every directory from `dir` down, reading each file and its RISC OS
/// fields; nothing it meets may panic.
fn walk(fs: &mut ArchiveFs<MemoryAccess>, dir: NodeId, depth: usize) {
    let mut entries = Vec::new();
    let _ = fs.read_dir(dir, 0, &[], &mut |entry, name| {
        entries.push((entry.node, entry.info.kind, name.to_vec()));
        DirVisit::Take
    });
    for (node, kind, name) in entries {
        let _ = fs.lookup(dir, &name);
        let mut value = [0u8; 64];
        let _ = fs.get_attr(node, b"acorn.filetype", &mut value);
        match kind {
            NodeKind::Directory if depth < 64 => walk(fs, node, depth + 1),
            NodeKind::RegularFile => {
                let size = fs.node_info(node).map_or(0, |info| info.size).min(READ_MAX);
                let mut buf = vec![0u8; 16 * 1024];
                let mut at = 0;
                while at < size {
                    match fs.read_at(node, at, &mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(got) => at += got as u64,
                    }
                }
            }
            NodeKind::Symlink => {
                let _ = fs.read_link(node, &mut [0u8; 4096]);
            }
            NodeKind::Directory => {}
        }
    }
}

/// One base: the archive, the name it is attached by, and the path of the
/// file a change makes in it, as its format names files.
struct Base {
    archive: Vec<u8>,
    name: &'static [u8],
    fresh: &'static [&'static [u8]],
}

/// The file a change makes in an archive of a format naming files freely.
const FRESH: &[&[u8]] = &[b"Fuzz"];

/// Open `archive`, walk it, and where it may be written, make the file
/// `fresh`, commit and walk what the commit wrote.
fn exercise(archive: &[u8], name: &[u8], fresh: &[&[u8]]) {
    let Some(Open {
        attachment,
        mut fs,
        writable,
    }) = open(archive, true, name)
    else {
        return;
    };
    let root = fs.root();
    walk(&mut fs, root, 0);
    if !writable {
        return;
    }
    let Some((leaf, dirs)) = fresh.split_last() else {
        return;
    };
    let dir = dirs
        .iter()
        .try_fold(root, |dir, part| fs.lookup(dir, part).ok());
    if let Some(file) =
        dir.and_then(|dir| fs.create(dir, leaf, NodeKind::RegularFile, &RECORD).ok())
    {
        let _ = fs.write_at(file, 0, b"fuzzed and committed");
    }
    if fs.flush().is_ok() {
        if let Some(mut again) = open(&attachment.archive(), false, name) {
            let root = again.fs.root();
            walk(&mut again.fs, root, 0);
        }
    }
}

/// `seed`, attached by `name` and written back by the driver with a
/// directory, a compressible and a stored file and a RISC OS filetype in it.
fn written(seed: &[u8], name: &[u8]) -> Vec<u8> {
    tairix_test_archive_image::written(seed, name, |fs| {
        let root = fs.root();
        let dir = fs.create(
            root,
            b"Dir",
            NodeKind::Directory,
            &NodeSecurity::new(0o755, 0, 0),
        )?;
        let text = fs.create(dir, b"Text", NodeKind::RegularFile, &RECORD)?;
        fs.write_all(text, 0, &b"compressible text, ".repeat(200))?;
        fs.set_attr(text, b"acorn.filetype", b"fff")?;
        let noise = fs.create(root, b"Noise", NodeKind::RegularFile, &RECORD)?;
        let noise_bytes: Vec<u8> = (0u32..300)
            .map(|at| at.wrapping_mul(2_654_435_761).to_le_bytes()[2])
            .collect();
        fs.write_all(noise, 0, &noise_bytes)
    })
    .expect("the seed archive is written back")
}

/// `seed`, a DFS disc attached by `name`, written back by the driver with
/// files on each side it has — one of them locked, at an I/O processor
/// address, one in a directory of its own, one crossing tracks — and one
/// removed again, leaving a gap.
fn written_disc(seed: &[u8], name: &[u8], sides: &[&[u8]]) -> Vec<u8> {
    tairix_test_archive_image::written(seed, name, |fs| {
        let root = fs.root();
        for side in sides {
            let dir = if side.is_empty() {
                root
            } else {
                fs.lookup(root, side)?
            };
            let code = fs.create(dir, b"$.CODE", NodeKind::RegularFile, &RECORD)?;
            fs.write_all(code, 0, &b"machine code ".repeat(400))?;
            fs.set_attr(code, b"acorn.loadaddr", b"ffff1900")?;
            fs.set_attr(code, b"acorn.attr", b"RWL/")?;
            let gap = fs.create(dir, b"$.GAP", NodeKind::RegularFile, &RECORD)?;
            fs.write_all(gap, 0, &[0x5A; 700])?;
            let data = fs.create(dir, b"B.DATA", NodeKind::RegularFile, &RECORD)?;
            fs.write_all(data, 0, b"data")?;
            fs.remove(dir, b"$.GAP")?;
        }
        Ok(())
    })
    .expect("the seed disc is written back")
}

/// The bases: each writable format's least archive written back by the
/// driver, a lone gzip stream, and the RAR fixtures.
fn bases() -> Vec<Base> {
    let seed = |format| tairix_test_archive_image::seed(format).expect("a seed");
    let base = |archive, name| Base {
        archive,
        name,
        fresh: FRESH,
    };
    let mut bases = vec![
        base(written(&seed(Format::Zip), b"seed.zip"), b"seed.zip"),
        base(written(&seed(Format::Tar), b"seed.tar"), b"seed.tar"),
        base(written(&seed(Format::TarGzip), b"seed.tgz"), b"seed.tgz"),
        base(written(&seed(Format::TarBzip2), b"seed.tbz2"), b"seed.tbz2"),
        base(
            tairix_test_archive_image::gzip(b"one file, compressed alone").expect("a stream"),
            b"lone.gz",
        ),
        base(written(&seed(Format::Spark), b"seed.spk"), b"seed.spk"),
        base(written(&seed(Format::ArcFs), b"seed.arc"), b"seed.arc"),
        Base {
            archive: written_disc(&seed(Format::Ssd), b"seed.ssd", &[b""]),
            name: b"seed.ssd",
            fresh: &[b"$.FUZZ"],
        },
        Base {
            archive: written_disc(&seed(Format::Dsd), b"seed.dsd", &[b"Drive 0", b"Drive 2"]),
            name: b"seed.dsd",
            fresh: &[b"Drive 2", b"$.FUZZ"],
        },
    ];
    let rar: [&[u8]; 11] = [
        include_bytes!("fixtures/rar/rarfile/rar15-comment.rar"),
        include_bytes!("fixtures/rar/rarfile/rar202-comment-nopsw.rar"),
        include_bytes!("fixtures/rar/rarfile/rar3-solid.rar"),
        include_bytes!("fixtures/rar/rarfile/rar3-symlink-unix.rar"),
        include_bytes!("fixtures/rar/rarfile/rar5-crc.rar"),
        include_bytes!("fixtures/rar/rarfile/rar5-dups.rar"),
        include_bytes!("fixtures/rar/synth/ppmd_o6.rar"),
        include_bytes!("fixtures/rar/synth/rgb_w30_p0.rar"),
        include_bytes!("fixtures/rar/synth/audio_ch2.rar"),
        include_bytes!("fixtures/rar/synth/delta_reuse.rar"),
        include_bytes!("fixtures/rar/synth/ppmd_o25.rar"),
    ];
    bases.extend(
        rar.into_iter()
            .map(|archive| base(archive.to_vec(), b"seed.rar")),
    );
    bases
}

#[test]
fn fuzz_archive_open() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "fuzz_archive_open",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let bases = bases();
    let parts = u32::try_from(bases.len()).unwrap();
    let slices = tairix_fuzzseed::budget_slices(deadline, parts);
    for (
        index,
        Base {
            archive: base,
            name,
            fresh,
        },
    ) in bases.iter().enumerate()
    {
        exercise(base, name, fresh);
        let until = slices(u32::try_from(index).unwrap());
        let mut iteration = 0u64;
        loop {
            let mut mutated = base.clone();
            match rng.below(4) {
                0 => {
                    for _ in 0..=rng.below(8) {
                        let at = rng.below(mutated.len());
                        mutated[at] ^= rng.edge_biased_u8().max(1);
                    }
                }
                1 => mutated.truncate(rng.below(mutated.len())),
                2 => {
                    let at = rng.below(mutated.len());
                    let end = (at + 1 + rng.below(32)).min(mutated.len());
                    for byte in &mut mutated[at..end] {
                        *byte = rng.next_u8();
                    }
                }
                _ => {
                    // A length or offset field pushed to an edge.
                    let at = rng.below(mutated.len().saturating_sub(4).max(1));
                    let edge = [0u32, 1, 0x7FFF_FFFF, 0xFFFF_FFFF][rng.below(4)];
                    let end = (at + 4).min(mutated.len());
                    mutated[at..end].copy_from_slice(&edge.to_le_bytes()[..end - at]);
                }
            }
            exercise(&mutated, name, fresh);
            iteration += 1;
            if !tairix_fuzzseed::within_budget(until) && iteration >= SMOKE_ITERATIONS {
                break;
            }
        }
    }
}
