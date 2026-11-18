//! The status band: its height, its slots, and how its text is set.

use alloc::vec::Vec;

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

#[test]
fn a_lamp_sits_on_its_slot_s_line_with_room_before_its_text() {
    let band = band();
    let lamp = band.lamp();
    let theme = Theme::light();
    let width = lamp.slot_width("DRIVE 0");
    let before_text = width - band.slot_width("DRIVE 0");
    let slot = Rect::new(4, 0, width, band.height());
    let text = lamp.text(slot);
    assert_eq!(
        text.left(),
        slot.left() + i32::try_from(before_text).expect("small")
    );
    assert_eq!(text.right(), slot.right());
    let mut surface = Surface::new(width + 8, band.height()).expect("a surface");
    lamp.paint(&mut surface, slot, &theme, StatusTone::Danger);
    let lit = Color::from(theme.palette().danger).premultiply();
    let columns: Vec<u32> = (0..surface.width())
        .filter(|&x| (0..surface.height()).any(|y| surface.get(x, y) == Some(lit)))
        .collect();
    let rows: Vec<u32> = (0..surface.height())
        .filter(|&y| (0..surface.width()).any(|x| surface.get(x, y) == Some(lit)))
        .collect();
    let (left, right) = (columns[0], columns[columns.len() - 1]);
    assert_eq!(left, 4, "at the slot's leading edge");
    assert!(
        i32::try_from(right).expect("small") < text.left() - 1,
        "room between the lamp and its text"
    );
    let (top, bottom) = (rows[0], rows[rows.len() - 1]);
    let line = band.face().centred_top(slot.top(), slot.height);
    let (first, last) = band
        .face()
        .capital_rows()
        .map(|(first, last)| {
            (
                line + i32::try_from(first).expect("small"),
                line + i32::try_from(last).expect("small"),
            )
        })
        .expect("the test face has capitals");
    assert_eq!(
        (
            i32::try_from(top).expect("small"),
            i32::try_from(bottom).expect("small")
        ),
        (first, last),
        "a capital's height, where the capitals stand"
    );
    let cramped = Rect::new(0, 0, 3, band.height());
    assert_eq!(
        lamp.text(cramped).width,
        0,
        "a slot no wider than its lamp has no text"
    );
}

#[test]
fn a_band_holding_a_button_is_tall_enough_for_it_and_centres_it() {
    let band = band();
    let theme = Theme::light();
    let height = band.height_holding_button(Scale::ONE, &theme);
    let button = crate::Button::height(Scale::ONE, &theme);
    assert!(height >= band.height() && height >= button);
    let slot = Rect::new(100, 50, 80, height);
    let placed = StatusBand::button_in(slot, Scale::ONE, &theme);
    assert_eq!(
        (placed.left(), placed.width, placed.height),
        (100, 80, button)
    );
    assert_eq!(placed.top() - slot.top(), slot.bottom() - placed.bottom());
    let short = Rect::new(0, 0, 80, button / 2);
    assert_eq!(
        StatusBand::button_in(short, Scale::ONE, &theme).height,
        button / 2
    );
}
