use super::*;

#[test]
fn a_single_sided_image_holds_its_side_in_order() {
    for offset in [0, 1, TRACK_LEN - 1, TRACK_LEN, 5 * TRACK_LEN + 17] {
        assert_eq!(Layout::Single.image_offset(0, offset), offset);
    }
}

#[test]
fn a_double_sided_image_alternates_its_sides_track_by_track() {
    let layout = Layout::Double;
    assert_eq!(layout.image_offset(0, 0), 0);
    assert_eq!(layout.image_offset(1, 0), TRACK_LEN);
    assert_eq!(layout.image_offset(0, TRACK_LEN), 2 * TRACK_LEN);
    assert_eq!(layout.image_offset(1, TRACK_LEN + 3), 3 * TRACK_LEN + 3);
    assert_eq!(layout.image_offset(0, TRACK_LEN - 1), TRACK_LEN - 1);
}

#[test]
fn a_short_image_holds_each_side_up_to_its_end() {
    let double = Layout::Double;
    assert_eq!(double.side_len(0, 0), 0);
    assert_eq!(double.side_len(0, 300), 300);
    assert_eq!(double.side_len(1, 300), 0);
    assert_eq!(double.side_len(0, TRACK_LEN + 300), TRACK_LEN);
    assert_eq!(double.side_len(1, TRACK_LEN + 300), 300);
    assert_eq!(double.side_len(0, 2 * TRACK_LEN), TRACK_LEN);
    assert_eq!(double.side_len(1, 2 * TRACK_LEN), TRACK_LEN);
    assert_eq!(double.side_len(0, 4 * TRACK_LEN + 5), 2 * TRACK_LEN + 5);
    assert_eq!(Layout::Single.side_len(0, 12_345), 12_345);
}

#[test]
fn an_image_holds_a_side_s_bytes_to_the_end_of_their_last_sector() {
    assert_eq!(Layout::Single.image_end(0, 0), 0);
    assert_eq!(Layout::Single.image_end(0, 1), SECTOR_LEN);
    assert_eq!(Layout::Single.image_end(0, SECTOR_LEN + 1), 2 * SECTOR_LEN);
    assert_eq!(Layout::Single.image_end(0, TRACK_LEN), TRACK_LEN);
    // The second side's track is reached through the first side's whole one.
    assert_eq!(
        Layout::Double.image_end(1, SECTOR_LEN),
        TRACK_LEN + SECTOR_LEN
    );
    assert_eq!(
        Layout::Double.image_end(0, TRACK_LEN + 1),
        2 * TRACK_LEN + SECTOR_LEN
    );
    assert_eq!(Layout::Double.image_end(1, 0), 0);
    for len in [1, 255, 256, 2559, 2560, 2561, 40_000] {
        for layout in [Layout::Single, Layout::Double] {
            for side in 0..layout.sides() {
                let image = layout.image_end(side, len);
                assert!(
                    layout.side_len(side, image) >= len,
                    "{layout:?} {side} {len}"
                );
                assert!(
                    layout.side_len(side, image - 1) < len.next_multiple_of(SECTOR_LEN),
                    "{layout:?} {side} {len}: no byte to spare"
                );
            }
        }
    }
}

#[test]
fn a_layout_is_named_by_its_ending_whatever_its_case() {
    assert_eq!(Layout::of_ending("ssd"), Some(Layout::Single));
    assert_eq!(Layout::of_ending("DSD"), Some(Layout::Double));
    assert_eq!(Layout::of_ending("adf"), None);
    for layout in [Layout::Single, Layout::Double] {
        assert_eq!(Layout::of_ending(layout.ending()), Some(layout));
    }
}
