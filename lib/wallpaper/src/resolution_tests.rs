//! Host tests of the desktop resolution's spelling and where it draws.

use super::Resolution;

#[test]
fn native_and_every_standard_size_round_trip() {
    assert_eq!(Resolution::from_value("native"), Some(Resolution::Native));
    for &(width, height) in tairix_geometry::STANDARD_SIZES {
        let size = Resolution::Size { width, height };
        assert_eq!(Resolution::from_value(&size.render_value()), Some(size));
    }
}

#[test]
fn a_size_no_output_could_offer_or_any_other_spelling_is_refused() {
    for refused in [
        "",
        "Native",
        "800X600",
        "800x",
        "x600",
        "801x600",
        "800 x 600",
        "+800x600",
        "0x0",
        "800x600x1",
        "1023x768",
    ] {
        assert_eq!(Resolution::from_value(refused), None, "{refused:?}");
    }
}

#[test]
fn a_size_is_drawn_only_where_the_output_offers_it() {
    let small = Resolution::Size {
        width: 800,
        height: 600,
    };
    assert_eq!(small.on((1024, 768)), (800, 600));
    assert_eq!(
        small.on((1920, 1080)),
        (1920, 1080),
        "a 4:3 size on a 16:9 output"
    );
    assert_eq!(small.on((640, 480)), (640, 480), "larger than the output");
    assert_eq!(Resolution::Native.on((1024, 768)), (1024, 768));
    assert_eq!(Resolution::of((1024, 768), (1024, 768)), Resolution::Native);
    assert_eq!(Resolution::of((800, 600), (1024, 768)), small);
}
