//! Hole-aware seek and punched holes: where a file's data and holes lie, and
//! a punch that releases a range through the copy-on-write free path.

use super::*;

/// A file of `cap`-byte records: data at the listed record indices, holes
/// elsewhere, `records` records long.
fn sparse_file(fs: &mut ARXFS<MemBlock>, name: &[u8], data: &[u64], records: u64) -> NodeId {
    let root = fs.root();
    fs.create(root, name, NodeKind::RegularFile)
        .expect("create");
    let cap = fs.data_capacity();
    fs.truncate(root, name, cap * records).expect("extend");
    for &record in data {
        // Distinct bytes per record, so dedupe cannot share them.
        let body = alloc::vec![u8::try_from(record % 250).expect("small") + 1; as_usize(cap)];
        fs.write_at(root, name, cap * record, &body).expect("write");
    }
    fs.lookup(root, name).expect("lookup")
}

/// Every boundary a seek reports for `node`, walking data then holes from
/// the start: the regions alternate, so this is the file's whole map.
fn region_map(fs: &mut ARXFS<MemBlock>, node: NodeId) -> Vec<(SeekRegion, u64)> {
    let mut map = Vec::new();
    let mut at = 0;
    let mut want = SeekRegion::Data;
    while let Some(next) = fs.seek_region(node, at, want).expect("seek") {
        map.push((want, next));
        at = next;
        want = match want {
            SeekRegion::Data => SeekRegion::Hole,
            SeekRegion::Hole => SeekRegion::Data,
        };
    }
    map
}

#[test]
fn a_seek_reports_every_data_run_and_hole_at_its_boundary() {
    let mut fs = fmt(512, 512, 64);
    let cap = fs.data_capacity();
    // Hole at 0, data 2..4, hole 4..6, data 6, hole 7..10.
    let node = sparse_file(&mut fs, b"f", &[2, 3, 6], 10);
    assert_eq!(
        region_map(&mut fs, node),
        [
            (SeekRegion::Data, 2 * cap),
            (SeekRegion::Hole, 4 * cap),
            (SeekRegion::Data, 6 * cap),
            (SeekRegion::Hole, 7 * cap),
        ]
    );
    assert_eq!(fs.seek_region(node, 0, SeekRegion::Hole), Ok(Some(0)));
    assert_eq!(
        fs.seek_region(node, 2 * cap + 5, SeekRegion::Data),
        Ok(Some(2 * cap + 5))
    );
    assert_eq!(
        fs.seek_region(node, 2 * cap + 5, SeekRegion::Hole),
        Ok(Some(4 * cap))
    );
    // Past the last data only the trailing hole remains, and past the end
    // nothing does.
    assert_eq!(fs.seek_region(node, 7 * cap, SeekRegion::Data), Ok(None));
    assert_eq!(fs.seek_region(node, 10 * cap, SeekRegion::Hole), Ok(None));
}

#[test]
fn a_file_that_is_all_hole_or_all_data_seeks_as_one_region() {
    let mut fs = fmt(512, 512, 64);
    let cap = fs.data_capacity();
    let hole = sparse_file(&mut fs, b"hole", &[], 5);
    assert_eq!(fs.seek_region(hole, 0, SeekRegion::Data), Ok(None));
    assert_eq!(fs.seek_region(hole, 0, SeekRegion::Hole), Ok(Some(0)));
    let solid = sparse_file(&mut fs, b"solid", &[0, 1, 2], 3);
    assert_eq!(fs.seek_region(solid, 0, SeekRegion::Data), Ok(Some(0)));
    // The end of a file always begins a hole.
    assert_eq!(
        fs.seek_region(solid, 0, SeekRegion::Hole),
        Ok(Some(3 * cap))
    );
    // A directory has no regions.
    let root = fs.root();
    assert_eq!(
        fs.seek_region(root, 0, SeekRegion::Data),
        Err(DriverError::Unsupported)
    );
}

#[test]
fn a_punch_reads_as_zeros_releases_its_blocks_and_keeps_the_length() {
    let mut fs = fmt(512, 512, 64);
    let cap = fs.data_capacity();
    let node = sparse_file(&mut fs, b"f", &[0, 1, 2, 3, 4, 5], 6);
    let ino = file_ino(&mut fs, b"f");
    let before = fs.stats().expect("stats").free_blocks;
    let root = fs.root();
    fs.punch(root, b"f", cap, 3 * cap).expect("punch");
    assert_eq!(mapped_block_count(&mut fs, ino), 3);
    // The three data blocks, at least: copy-on-write metadata moves too.
    assert!(fs.stats().expect("stats").free_blocks >= before + 3);
    assert_extents_ordered_and_disjoint(&mut fs, ino);
    assert_eq!(fs.node_info(node).expect("info").size, 6 * cap);
    let body = read_all(&mut fs, node, as_usize(6 * cap));
    let capu = as_usize(cap);
    assert!(body[capu..4 * capu].iter().all(|&b| b == 0));
    assert!(body[..capu].iter().all(|&b| b == 1));
    assert!(body[4 * capu..5 * capu].iter().all(|&b| b == 5));
    assert_eq!(
        region_map(&mut fs, node),
        [
            (SeekRegion::Data, 0),
            (SeekRegion::Hole, cap),
            (SeekRegion::Data, 4 * cap),
            (SeekRegion::Hole, 6 * cap),
        ]
    );
}

#[test]
fn a_punch_zeroes_the_partial_ends_in_place_and_merges_adjacent_holes() {
    let mut fs = fmt(512, 512, 64);
    let cap = fs.data_capacity();
    let node = sparse_file(&mut fs, b"f", &[0, 1, 3], 4);
    let ino = file_ino(&mut fs, b"f");
    let root = fs.root();
    // From the middle of record 0 into the middle of record 1: record 0 keeps
    // its head, record 1 becomes all zeros and so a hole beside record 2's.
    fs.punch(root, b"f", cap / 2, cap + cap / 2 + 3)
        .expect("punch");
    let body = read_all(&mut fs, node, as_usize(4 * cap));
    let half = as_usize(cap / 2);
    assert!(body[..half].iter().all(|&b| b == 1));
    assert!(body[half..as_usize(2 * cap) + 3].iter().all(|&b| b == 0));
    assert!(body[as_usize(2 * cap) + 3..as_usize(3 * cap)]
        .iter()
        .all(|&b| b == 0));
    assert_eq!(mapped_block_count(&mut fs, ino), 2);
    fs.punch(root, b"f", 0, cap)
        .expect("punch the rest of record 0");
    assert_eq!(fs.seek_region(node, 0, SeekRegion::Data), Ok(Some(3 * cap)));
    assert_eq!(mapped_block_count(&mut fs, ino), 1);
}

#[test]
fn a_punch_past_the_end_empty_or_on_a_hole_changes_nothing_it_should_not() {
    let mut fs = fmt(512, 512, 64);
    let cap = fs.data_capacity();
    let node = sparse_file(&mut fs, b"f", &[0, 1], 3);
    let root = fs.root();
    // Spanning the end releases what the file holds and never grows it.
    fs.punch(root, b"f", cap, u64::MAX - cap)
        .expect("punch to the end");
    assert_eq!(fs.node_info(node).expect("info").size, 3 * cap);
    assert_eq!(fs.seek_region(node, 0, SeekRegion::Hole), Ok(Some(cap)));
    let gen = fs.node_info(node).expect("info").content_gen;
    fs.punch(root, b"f", 0, 0).expect("an empty punch");
    fs.punch(root, b"f", 5 * cap, cap)
        .expect("a punch past the end");
    assert_eq!(fs.node_info(node).expect("info").content_gen, gen);
    assert_eq!(
        fs.punch(root, b"f", u64::MAX, 2),
        Err(DriverError::OutOfRange)
    );
    assert_eq!(fs.punch(root, b"nope", 0, 1), Err(DriverError::NotFound));
    let dir = fs.create(root, b"d", NodeKind::Directory).expect("mkdir");
    let _ = dir;
    assert_eq!(fs.punch(root, b"d", 0, 1), Err(DriverError::Unsupported));
}

#[test]
fn a_punch_keeps_a_reflinked_copy_and_frees_only_unshared_blocks() {
    let mut fs = fmt(512, 512, 64);
    let cap = fs.data_capacity();
    let node = sparse_file(&mut fs, b"f", &[0, 1], 2);
    let root = fs.root();
    let original = read_all(&mut fs, node, as_usize(2 * cap));
    fs.reflink(root, b"f", b"snap").expect("reflink");
    fs.punch(root, b"f", 0, 2 * cap).expect("punch");
    assert_eq!(fs.seek_region(node, 0, SeekRegion::Data), Ok(None));
    // Every block is still the snapshot's, so the snapshot still maps and
    // reads all of it.
    let snap = fs.lookup(root, b"snap").expect("snap");
    let snap_ino = file_ino(&mut fs, b"snap");
    assert_eq!(mapped_block_count(&mut fs, snap_ino), 2);
    assert_eq!(read_all(&mut fs, snap, as_usize(2 * cap)), original);
    // Once no other file shares them, a punch frees them.
    let before = fs.stats().expect("stats").free_blocks;
    fs.punch(root, b"snap", 0, 2 * cap)
        .expect("punch the last referrer");
    assert!(fs.stats().expect("stats").free_blocks >= before + 2);
}

#[test]
fn a_punch_on_a_read_only_volume_is_refused() {
    let mut fs = fmt(512, 512, 64);
    sparse_file(&mut fs, b"f", &[0], 1);
    let bytes = fs.into_block().expect("the volume closes").bytes();
    let mut fs =
        ARXFS::open_read_only(MemBlock::from_bytes(bytes, 512, 512), TEST_KEYING).expect("open");
    let root = fs.root();
    assert_eq!(fs.punch(root, b"f", 0, 1), Err(DriverError::ReadOnly));
}

#[test]
fn a_punch_over_many_extents_spans_bounded_transactions() {
    let mut fs = fmt(512, 4096, 64);
    let cap = fs.data_capacity();
    // Every other record holds data, so each is its own extent.
    let data: Vec<u64> = (0..400).step_by(2).collect();
    let node = sparse_file(&mut fs, b"f", &data, 400);
    let ino = file_ino(&mut fs, b"f");
    let root = fs.root();
    fs.punch(root, b"f", 0, 400 * cap)
        .expect("punch everything");
    assert_eq!(mapped_block_count(&mut fs, ino), 0);
    assert_eq!(fs.seek_region(node, 0, SeekRegion::Data), Ok(None));
    // And the release survives a remount: the transactions were published.
    let bytes = fs.into_block().expect("the volume closes").bytes();
    let mut fs = ARXFS::open(MemBlock::from_bytes(bytes, 512, 4096), TEST_KEYING).expect("reopen");
    let ino = file_ino(&mut fs, b"f");
    assert_eq!(mapped_block_count(&mut fs, ino), 0);
}
