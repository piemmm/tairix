extern crate std;

use std::cell::Cell;
use std::vec::Vec;

use super::*;
use crate::hostmem::HostFrames;

const PAGE: u64 = IO_PAGE_SIZE;
const MIB2: u64 = 2 << 20;
const GIB: u64 = 1 << 30;
const ADDRESS: u64 = 0x000F_FFFF_FFFF_F000;
const LARGE: u64 = 1 << 7;
const READ_WRITE: u64 = 0b11;

/// A format shaped like VT-d's second level: read and write bits, and a
/// page-size bit on a leaf above level 0.
struct TestFormat {
    large_leaves: bool,
}

impl PteFormat for TestFormat {
    fn leaf_allowed(&self, level: u32) -> bool {
        level == 0 || (self.large_leaves && level <= 2)
    }

    fn table(&self, phys: u64, _level: u32) -> u64 {
        phys | READ_WRITE
    }

    fn leaf(&self, phys: u64, level: u32, access: Access) -> u64 {
        let size = if level > 0 { LARGE } else { 0 };
        phys | u64::from(access.read()) | (u64::from(access.write()) << 1) | size
    }

    fn decode(&self, entry: u64, level: u32) -> Pte {
        if entry & READ_WRITE == 0 {
            return Pte::Absent;
        }
        if level == 0 || entry & LARGE != 0 {
            let access = match entry & READ_WRITE {
                0b01 => Access::READ,
                0b10 => Access::WRITE,
                _ => Access::READ_WRITE,
            };
            Pte::Leaf(entry & ADDRESS, access)
        } else {
            Pte::Table(entry & ADDRESS)
        }
    }
}

fn tree(frames: &HostFrames, levels: u32, large_leaves: bool) -> IoPageTable<'_, TestFormat> {
    IoPageTable::new(TestFormat { large_leaves }, levels, frames, None).unwrap()
}

#[test]
fn a_page_maps_translates_and_unmaps_leaving_only_the_root() {
    let frames = HostFrames::new(0x1000_0000);
    let mut table = tree(&frames, 4, true);
    table
        .map(0x7000, 0x8_0000_0000, PAGE, Access::READ_WRITE)
        .unwrap();
    assert_eq!(
        table.translate(0x7123),
        Some((0x8_0000_0123, Access::READ_WRITE))
    );
    assert_eq!(table.translate(0x8000), None);
    assert_eq!(frames.live(), 4, "the root and one table per lower level");
    table.unmap(0x7000, PAGE).unwrap();
    assert_eq!(table.translate(0x7000), None);
    assert!(table.has_retired());
    assert_eq!(frames.live(), 4, "emptied tables wait for the sync");
    table.release_retired();
    assert_eq!(frames.live(), 1);
}

#[test]
fn an_aligned_range_takes_the_largest_leaves_the_format_allows() {
    let frames = HostFrames::new(0x1000_0000);
    let mut table = tree(&frames, 4, true);
    table
        .map(4 * MIB2, 8 * MIB2, MIB2, Access::READ_WRITE)
        .unwrap();
    assert_eq!(frames.live(), 3, "a 2 MiB leaf needs no level-0 table");
    assert_eq!(
        table.translate(4 * MIB2 + 0x1_2345),
        Some((8 * MIB2 + 0x1_2345, Access::READ_WRITE))
    );

    let small = HostFrames::new(0x2000_0000);
    let mut paged = tree(&small, 4, false);
    paged
        .map(4 * MIB2, 8 * MIB2, MIB2, Access::READ_WRITE)
        .unwrap();
    assert_eq!(small.live(), 4);
    assert_eq!(
        paged.translate(4 * MIB2 + MIB2 - 1),
        Some((9 * MIB2 - 1, Access::READ_WRITE))
    );
}

#[test]
fn a_gigabyte_leaf_sits_one_level_higher() {
    let frames = HostFrames::new(0x1000_0000);
    let mut table = tree(&frames, 4, true);
    table.map(GIB, 3 * GIB, GIB, Access::READ).unwrap();
    assert_eq!(frames.live(), 2);
    assert_eq!(table.translate(GIB + 7), Some((3 * GIB + 7, Access::READ)));
}

#[test]
fn misaligned_physical_memory_falls_back_to_pages() {
    let frames = HostFrames::new(0x1000_0000);
    let mut table = tree(&frames, 4, true);
    table
        .map(MIB2, MIB2 + PAGE, MIB2, Access::READ_WRITE)
        .unwrap();
    assert_eq!(frames.live(), 4);
    assert_eq!(
        table.translate(MIB2 + MIB2 - PAGE),
        Some((2 * MIB2, Access::READ_WRITE))
    );
}

#[test]
fn a_range_straddling_a_table_boundary_spans_two_tables() {
    let frames = HostFrames::new(0x1000_0000);
    let mut table = tree(&frames, 4, true);
    table
        .map(MIB2 - 2 * PAGE, 0x40_0000, 4 * PAGE, Access::READ_WRITE)
        .unwrap();
    assert_eq!(frames.live(), 5);
    assert_eq!(
        table.translate(MIB2 + PAGE),
        Some((0x40_3000, Access::READ_WRITE))
    );
    table.unmap(MIB2 - 2 * PAGE, 4 * PAGE).unwrap();
    table.release_retired();
    assert_eq!(frames.live(), 1);
}

#[test]
fn mapping_over_a_mapping_is_refused_and_leaves_it_intact() {
    let frames = HostFrames::new(0x1000_0000);
    let mut table = tree(&frames, 4, true);
    table
        .map(MIB2, 0x10_0000_0000, MIB2, Access::READ_WRITE)
        .unwrap();
    assert_eq!(
        table.map(MIB2 + PAGE, 0x9000, PAGE, Access::READ),
        Err(IommuError::AlreadyMapped)
    );
    assert_eq!(
        table.map(MIB2, 0x10_0000_0000, MIB2, Access::READ),
        Err(IommuError::AlreadyMapped)
    );
    assert_eq!(
        table.translate(MIB2 + PAGE),
        Some((0x10_0000_1000, Access::READ_WRITE))
    );
}

#[test]
fn an_unmap_never_splits_a_leaf_or_crosses_a_hole() {
    let frames = HostFrames::new(0x1000_0000);
    let mut table = tree(&frames, 4, true);
    table
        .map(MIB2, 0x10_0000_0000, MIB2, Access::READ_WRITE)
        .unwrap();
    assert_eq!(table.unmap(MIB2 + PAGE, PAGE), Err(IommuError::Split));
    assert_eq!(
        table.translate(MIB2),
        Some((0x10_0000_0000, Access::READ_WRITE))
    );
    assert_eq!(table.unmap(0x10_0000, PAGE), Err(IommuError::NotMapped));
    table.unmap(MIB2, MIB2).unwrap();
}

#[test]
fn running_out_of_tables_undoes_what_the_map_installed() {
    let frames = HostFrames::new(0x1000_0000);
    let mut table = tree(&frames, 4, false);
    // Room for the path to the first level-0 table and no more: the map
    // across the boundary gets its first page in and then fails.
    frames.limit(3);
    assert_eq!(
        table.map(MIB2 - PAGE, 0x9000_0000, 2 * PAGE, Access::READ_WRITE),
        Err(IommuError::Exhausted)
    );
    assert_eq!(table.translate(MIB2 - PAGE), None);
    table.release_retired();
    assert_eq!(frames.live(), 1);
}

#[test]
fn a_bad_range_is_refused_before_anything_changes() {
    let frames = HostFrames::new(0x1000_0000);
    let mut table = tree(&frames, 3, true);
    assert_eq!(table.input_bits(), 39);
    for (iova, phys, len) in [
        (0x1001, 0x2000, PAGE),
        (0x1000, 0x2001, PAGE),
        (0x1000, 0x2000, 0),
        (0x1000, 0x2000, PAGE + 1),
        (1 << 39, 0x2000, PAGE),
        ((1 << 39) - PAGE, 0x2000, 2 * PAGE),
        (0x1000, !(PAGE - 1), 2 * PAGE),
    ] {
        assert_eq!(
            table.map(iova, phys, len, Access::READ),
            Err(IommuError::OutOfRange),
            "{iova:#x} {phys:#x} {len:#x}"
        );
    }
    assert_eq!(frames.live(), 1);
    assert_eq!(
        IoPageTable::new(TestFormat { large_leaves: true }, 0, &frames, None).err(),
        Some(IommuError::OutOfRange)
    );
    assert_eq!(
        IoPageTable::new(TestFormat { large_leaves: true }, 7, &frames, None).err(),
        Some(IommuError::OutOfRange)
    );
}

#[test]
fn every_depth_translates_its_whole_reach() {
    for levels in 1..=MAX_LEVELS {
        let frames = HostFrames::new(0x1000_0000);
        let mut table = tree(&frames, levels, true);
        let reach_bits = (IO_PAGE_SHIFT + 9 * levels).min(63);
        let top = (1u64 << reach_bits) - PAGE;
        table.map(top, 0x5000, PAGE, Access::READ_WRITE).unwrap();
        assert_eq!(table.translate(top + 8), Some((0x5008, Access::READ_WRITE)));
        assert_eq!(u32::try_from(frames.live()).unwrap(), levels);
    }
}

#[test]
fn dropping_the_tree_frees_every_table() {
    let frames = HostFrames::new(0x1000_0000);
    {
        let mut table = tree(&frames, 4, false);
        table
            .map(0, 0x1_0000_0000, 8 * MIB2, Access::READ_WRITE)
            .unwrap();
        table
            .map(GIB, 0x2_0000_0000, PAGE, Access::READ_WRITE)
            .unwrap();
        table.unmap(GIB, PAGE).unwrap();
        assert!(frames.live() > 1);
    }
    assert_eq!(frames.live(), 0);
}

/// A walker that does not snoop sees every table line written back, and the
/// zeroed table before the entry that points at it.
#[test]
fn a_non_snooping_walker_has_every_touched_line_written_back() {
    struct Recorder(Cell<Vec<(u64, usize)>>);
    // SAFETY: the test drives the recorder from one thread.
    unsafe impl Sync for Recorder {}
    impl TableCoherence for Recorder {
        fn write_back(&self, phys: u64, len: usize) {
            let mut log = self.0.take();
            log.push((phys, len));
            self.0.set(log);
        }
    }
    let frames = HostFrames::new(0x1000_0000);
    let recorder = Recorder(Cell::new(Vec::new()));
    let mut table = IoPageTable::new(
        TestFormat { large_leaves: true },
        2,
        &frames,
        Some(&recorder),
    )
    .unwrap();
    let root = table.root();
    table.map(0x3000, 0x7000, PAGE, Access::READ_WRITE).unwrap();
    let log = recorder.0.take();
    assert_eq!(log[0], (root, 4096), "the root is written back whole");
    let child = log[1].0;
    assert_eq!(log[1], (child, 4096), "the new table before it is linked");
    assert_eq!(log[2], (root, 8), "then the entry pointing at it");
    assert_eq!(log[3], (child + 3 * 8, 8), "then the leaf");
    assert_eq!(log.len(), 4);
}
