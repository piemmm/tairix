use super::*;

#[test]
fn a_pointer_image_packs_four_pixels_a_byte_the_leftmost_lowest() {
    let shape = image(
        (5, 2),
        Scale::UNIT,
        |column, row| Some(column + row),
        false,
        (3, 4),
    );
    assert_eq!((shape.width, shape.height, shape.active), (2, 2, (3, 4)));
    assert_eq!(shape.data, [0b1110_0100, 0, 0b0011_1001, 1]);
}

#[test]
fn a_clear_pixel_is_colour_nought_and_the_first_solid_one_the_active_point() {
    let shape = image(
        (4, 2),
        Scale::UNIT,
        |column, row| (row == 1 && column >= 2).then_some(3),
        true,
        (0, 0),
    );
    assert_eq!(shape.active, (2, 1));
    assert_eq!(shape.data, [0, 0b1111_0000]);
}

#[test]
fn a_pointer_image_is_cut_to_32_pixels_each_way() {
    let shape = image((40, 50), Scale::UNIT, |_, _| Some(1), false, (0, 0));
    assert_eq!((shape.width, shape.height, shape.data.len()), (8, 32, 256));
}
