use super::*;

#[test]
fn a_mask_covers_the_bits_from_low_to_high() {
    assert_eq!(mask_between(0, 32), u32::MAX);
    assert_eq!(mask_between(4, 8), 0xF0);
    assert_eq!(mask_between(0, 0), 0);
    assert_eq!(mask_between(31, 32), 0x8000_0000);
}

#[test]
fn a_block_s_row_follows_the_line_less_the_ecf_offset() {
    let mut block = Block::solid(0, 0);
    for (row, i) in block.rows.iter_mut().zip(0u32..) {
        *row = [i, 0];
    }
    block.y_offset = 3;
    assert_eq!(block.for_line(3), (0, 0));
    assert_eq!(block.for_line(10), (7, 0));
    assert_eq!(
        block.for_line(2),
        (7, 0),
        "a line above the offset wraps round"
    );
}

#[test]
fn a_canvas_lays_its_lines_one_after_another() {
    let canvas = Canvas {
        start: 0x1000,
        line: 80,
        width: 640,
        height: 480,
        log2_bpc: 0,
    };
    assert_eq!(canvas.address(2, 5), 0x1000 + 165);
    assert_eq!(canvas.row_of(0), 479, "the bottom line");
    assert_eq!(canvas.row_of(479), 0);
}

#[test]
fn pixel_bits_are_read_and_written_lowest_first() {
    let mut bytes = [0u8; 4];
    set_bits_at(&mut bytes, 2, 2, 3);
    assert_eq!(bytes[0], 0b1100);
    assert_eq!(bits_at(&bytes, 2, 2), 3);
    set_bits_at(&mut bytes, 8, 16, 0xBEEF);
    assert_eq!(bits_at(&bytes, 8, 16), 0xBEEF);
}
