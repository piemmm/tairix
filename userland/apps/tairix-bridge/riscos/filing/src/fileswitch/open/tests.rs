//! The pure rules of open files: which blocks move straight between a filing
//! system and the caller, the stamp a written file takes, and handles.

use super::{direct, new_stamp, Buffer, Files, HANDLES};

fn buffer(extent: u32) -> Buffer {
    Buffer {
        size: 256,
        at: 0,
        held: None,
        dirty: false,
        pointer: 0,
        extent,
        allocated: 1024,
    }
}

#[test]
fn whole_blocks_within_the_extent_move_straight() {
    let b = buffer(1000);
    assert_eq!(direct(&b, 0, 512, b.extent), Some((0, 512)));
    assert_eq!(direct(&b, 0, 1000, b.extent), Some((0, 768)));
    assert_eq!(direct(&b, 0, 256, b.extent), Some((0, 256)));
}

#[test]
fn a_transfer_whose_blocks_run_past_the_extent_moves_none_straight() {
    let b = buffer(1000);
    assert_eq!(direct(&b, 500, 600, b.extent), None);
    assert_eq!(direct(&b, 601, 300, b.extent), None);
}

#[test]
fn a_write_moves_its_whole_blocks_straight_wherever_they_lie() {
    let b = buffer(1000);
    assert_eq!(direct(&b, 256, 256, u32::MAX), Some((256, 256)));
    assert_eq!(direct(&b, 100, 1000, u32::MAX), Some((256, 768)));
    assert_eq!(direct(&b, 0, 300, u32::MAX), Some((0, 256)));
}

#[test]
fn an_offset_lies_in_the_block_its_size_aligns_it_to() {
    let b = buffer(1000);
    assert_eq!(b.block(0), 0);
    assert_eq!(b.block(255), 0);
    assert_eq!(b.block(601), 512);
}

#[test]
fn a_written_file_keeps_its_type_or_becomes_data_or_keeps_its_addresses() {
    let now = 0x5D_393A_D30C;
    assert_eq!(
        new_stamp((0xFFFF_FB12, 0x3456_7890), now),
        (0xFFFF_FB5D, 0x393A_D30C)
    );
    assert_eq!(new_stamp((0, 0), now), (0xFFFF_FD5D, 0x393A_D30C));
    assert_eq!(new_stamp((0x8000, 0x8040), now), (0, 0));
}

#[test]
fn no_handle_is_open_on_an_empty_table() {
    let files = Files::new();
    assert_eq!(files.stream(HANDLES), None);
    assert_eq!(files.stream(0), None);
    assert_eq!(files.stream(HANDLES + 1), None);
    assert!(files.handles(None).is_empty());
}
