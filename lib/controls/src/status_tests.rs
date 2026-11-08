//! The status band: its height, its slots, and how its text is set.

use tairix_geometry::{Rect, Scale};
use tairix_raster::{Color, Surface};
use tairix_theme::Theme;

use super::{StatusBand, StatusTone};
use crate::testkit::{ink_over, marks_elision};

fn band() -> StatusBand {
    StatusBand::new(&Theme::light(), Scale::ONE)
}

#[test]
fn a_band_is_a_line_of_its_face_with_a_gap_either_side() {
    let band = band();
    let gap = Scale::ONE
        .scale_length(Theme::light().metrics().control_gap)
        .max(1);
    assert_eq!(band.height(), band.face().line_height() + gap * 2);
    assert_eq!(
        band.slot_width("100%"),
        band.face().text_width("100%") + gap * 2
    );
    let doubled = StatusBand::new(&Theme::light(), Scale::from_percent(200).expect("a scale"));
    assert!(doubled.height() > band.height(), "it grows with the scale");
}

#[test]
fn slots_are_cut_from_either_edge_and_the_message_takes_the_rest() {
    let band = band();
    let gap = (band.height() - band.face().line_height()) / 2;
    let whole = Rect::new(10, 400, 600, band.height());
    let slots = band.slots(whole, [80], [50, 40]);
    assert_eq!(
        slots.leading[0].left(),
        whole.left() + i32::try_from(gap).expect("small")
    );
    assert_eq!(slots.leading[0].width, 80);
    assert_eq!(
        slots.trailing[0].right(),
        whole.right() - i32::try_from(gap).expect("small")
    );
    assert_eq!(
        slots.trailing[0].width, 50,
        "the first trailing slot is outermost"
    );
    assert_eq!(slots.trailing[1].right(), slots.trailing[0].left());
    assert_eq!(slots.message.left(), slots.leading[0].right());
    assert_eq!(slots.message.right(), slots.trailing[1].left());
    assert_eq!(slots.message.width, 600 - gap * 2 - 80 - 50 - 40);
    for slot in [
        slots.leading[0],
        slots.message,
        slots.trailing[0],
        slots.trailing[1],
    ] {
        assert_eq!((slot.top(), slot.height), (whole.top(), whole.height));
    }

    let cramped = band.slots(Rect::new(0, 0, 100, band.height()), [80], [70]);
    assert_eq!(cramped.trailing[0].width, 70, "the trailing slots first");
    assert_eq!(
        cramped.leading[0].width,
        100 - gap * 2 - 70,
        "the leading ones in what is left"
    );
    assert!(cramped.message.is_empty());
}

#[test]
fn a_band_is_plated_and_its_text_toned_and_elided_to_its_slot() {
    let band = band();
    let theme = Theme::light();
    let whole = Rect::new(0, 0, 240, band.height());
    let slots = band.slots(whole, [], [60]);
    let paint = |text: &str, tone: StatusTone| {
        let mut surface = Surface::new(240, band.height()).expect("a surface");
        band.paint(
            &mut surface,
            whole,
            &theme,
            &[(slots.message, text, tone), (slots.trailing[0], "", tone)],
        );
        surface
    };
    let plain = paint("", StatusTone::Ink);
    let raised = Color::from(theme.palette().surface_raised).premultiply();
    assert!(
        plain.pixels().iter().all(|pixel| *pixel == raised),
        "the plate"
    );
    let said = paint("Saved", StatusTone::Ink);
    assert!(ink_over(&said, &plain) > 0);
    let ink = Color::from(theme.palette().on_surface).premultiply();
    let danger = Color::from(theme.palette().danger).premultiply();
    assert!(said.pixels().contains(&ink));
    assert!(paint("Saved", StatusTone::Danger)
        .pixels()
        .contains(&danger));
    let message = slots.message;
    let spilt = paint(&"W".repeat(200), StatusTone::Ink);
    let past = (0..spilt.height()).any(|y| {
        (u32::try_from(message.right()).expect("on")..spilt.width())
            .any(|x| spilt.get(x, y) != plain.get(x, y))
    });
    assert!(!past, "nothing is drawn past the message's slot");
    assert!(marks_elision(|text| paint(text, StatusTone::Ink)));
}
