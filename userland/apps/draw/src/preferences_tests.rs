//! Draw's settings: every key spelt and read back, what a store's value is
//! refused for, and the page, grid, style and handling they make.

#![allow(
    clippy::float_cmp,
    reason = "the lengths compared are hundredths over a hundred, exact as stated"
)]

use alloc::string::String;

use tairix_appconf::Registry;
use tairix_colour::Rgba;
use tairix_drawing::{Paint, Unit, PAPER};

use super::{token_of, PrefKey, Preferences, MOST_LENGTH, PICK_REACH};
use crate::tools::Tool;
use crate::view::Showing;

/// `record` spelt key by key and read back into the defaults.
fn round_tripped(record: &Preferences) -> Preferences {
    let mut read = Preferences::default();
    let mut text = String::new();
    for &key in Preferences::KEYS {
        text.clear();
        assert!(record.spell(key, &mut text), "{key:?} spells");
        assert!(read.read(key, &text), "{key:?} reads {text:?} back");
    }
    read
}

#[test]
fn every_setting_reads_back_as_it_was_spelt() {
    assert_eq!(
        round_tripped(&Preferences::default()),
        Preferences::default()
    );
    let record = Preferences {
        tool: Tool::Star,
        show: Showing {
            outline: true,
            grid: true,
            snapping: false,
        },
        paper: PAPER.len() - 1,
        landscape: true,
        unit: Unit::In,
        grid_spacing: 4800,
        subdivisions: 8,
        fill: None,
        stroke: Some(Rgba::new(1, 2, 3, 0x80)),
        stroke_width: 25,
        nudge: 50,
        duplicate_offset: 0,
        pick: PICK_REACH.1,
    };
    assert_eq!(round_tripped(&record), record);
}

#[test]
fn a_value_a_setting_does_not_accept_is_refused() {
    let mut record = Preferences::default();
    for (key, text) in [
        (PrefKey::Tool, "lasso"),
        (PrefKey::Paper, "a7"),
        (PrefKey::Unit, "furlong"),
        (PrefKey::Fill, "#ff0000"),
        (PrefKey::Fill, "red"),
        (PrefKey::Subdivisions, "0"),
        (PrefKey::Subdivisions, "65"),
        (PrefKey::Nudge, "0"),
        (PrefKey::StrokeWidth, "1000001"),
        (PrefKey::Pick, "1"),
        (PrefKey::Grid, "yes"),
    ] {
        assert!(!record.read(key, text), "{key:?} refuses {text:?}");
    }
    assert_eq!(
        record,
        Preferences::default(),
        "a refusal leaves it as it was"
    );
    assert!(record.read(PrefKey::StrokeWidth, "1000000"));
    assert_eq!(record.stroke_width, MOST_LENGTH);
    assert!(record.read(PrefKey::Tool, "Star and polygon"), "any case");
    assert!(record.read(PrefKey::Fill, "none"));
    assert_eq!(record.fill, None);
}

#[test]
fn a_paper_is_spelt_as_its_name_in_lower_case_words() {
    assert_eq!(token_of("A4"), "a4");
    assert_eq!(token_of("US Letter"), "us-letter");
    assert_eq!(token_of("Screen 1920 × 1080"), "screen-1920-x-1080");
    let mut tokens: alloc::vec::Vec<String> =
        PAPER.iter().map(|paper| token_of(paper.name)).collect();
    tokens.sort();
    tokens.dedup();
    assert_eq!(tokens.len(), PAPER.len(), "every paper its own spelling");
}

#[test]
fn a_new_drawing_takes_its_paper_turned_and_measured_as_set() {
    let mut record = Preferences::default();
    let a4 = record.page();
    assert_eq!((a4.width, a4.height, a4.unit), (210.0, 297.0, Unit::Mm));
    record.landscape = true;
    record.unit = Unit::Cm;
    let turned = record.page();
    assert_eq!(turned.unit, Unit::Cm);
    assert!((turned.width - 29.7).abs() < 1e-9 && (turned.height - 21.0).abs() < 1e-9);
    record.grid_spacing = 2500;
    record.subdivisions = 5;
    let grid = record.grid();
    assert_eq!((grid.spacing, grid.subdivisions), (25.0, 5));
}

#[test]
fn a_new_window_draws_in_the_style_and_moves_as_set() {
    let record = Preferences {
        fill: None,
        stroke: Some(Rgba::new(10, 20, 30, 255)),
        stroke_width: 250,
        nudge: 50,
        duplicate_offset: 1250,
        pick: 9,
        ..Preferences::default()
    };
    let style = record.style();
    assert_eq!(style.fill, Paint::None);
    assert_eq!(
        style.stroke.paint,
        Paint::Colour(Rgba::new(10, 20, 30, 255))
    );
    assert_eq!(style.stroke.width, 2.5);
    let handling = record.handling();
    assert_eq!(
        (handling.nudge, handling.duplicate_offset, handling.pick),
        (0.5, 12.5, 9)
    );
}
