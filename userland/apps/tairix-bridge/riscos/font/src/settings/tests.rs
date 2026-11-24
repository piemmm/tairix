use super::*;

#[test]
fn millipoints_floor_to_a_whole_pixel_of_the_mode_as_risc_os_5_30_s_do() {
    for (millipoints, eigen, os) in [
        (1000, 1, 2),
        (2000, 1, 4),
        (399, 1, 0),
        (401, 1, 0),
        (-399, 1, -2),
        (-401, 1, -2),
        (1000, 2, 0),
        (3000, 2, 4),
        (-1, 2, -4),
    ] {
        assert_eq!(
            to_os(millipoints, 400, eigen),
            os,
            "{millipoints} at eigen {eigen}"
        );
    }
    assert_eq!(to_os(1000, 500, 1), 2);
    assert_eq!(to_os(1000, 300, 1), 2);
    assert_eq!(to_os(7, -2, 0), -4, "a negative scale floors too");
    assert_eq!(to_os(1000, 0, 1), 0, "a scale of nought gives nought");
    assert_eq!(to_os(i32::MIN, 1, 1), i32::MIN);
}
