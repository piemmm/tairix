use alloc::string::String;
use alloc::vec;

use tairix_geometry::{Rect, Scale};
use tairix_icon::IconKind;
use tairix_raster::Surface;
use tairix_theme::Theme;

use super::{AboutSheet, ABOUT_MAX_WIDTH, ABOUT_MIN_WIDTH};
use crate::record::{Fact, FactList};

fn sheet(purpose: Option<&str>) -> AboutSheet {
    AboutSheet::new(
        "Paint",
        "1.4.0",
        purpose.map(String::from),
        FactList::new(vec![Fact::new("Author", "TAIRiX")]),
        IconKind::AppBundle,
    )
}

#[test]
fn the_sheet_is_titled_about_its_application() {
    let about = sheet(None);
    assert_eq!(about.band.title(), "About Paint");
    assert_eq!((about.name(), about.version()), ("Paint", "1.4.0"));
}

#[test]
fn the_sheet_is_held_between_its_floor_and_ceiling() {
    let theme = Theme::dark();
    let (narrow, _) = sheet(None).measured_size(Scale::ONE, &theme);
    assert_eq!(narrow, ABOUT_MIN_WIDTH, "a short identity takes the floor");
    let long = AboutSheet::new(
        "An application whose name runs on for a very long way indeed",
        "1",
        None,
        FactList::new(vec![]),
        IconKind::AppBundle,
    );
    let (wide, _) = long.measured_size(Scale::ONE, &theme);
    assert_eq!(wide, ABOUT_MAX_WIDTH, "a long one wraps under the ceiling");
}

#[test]
fn a_purpose_is_prose_that_grows_the_sheet_and_none_closes_up() {
    let theme = Theme::dark();
    let (_, without) = sheet(None).measured_size(Scale::ONE, &theme);
    let (_, with) = sheet(Some(
        "Draws and edits pictures: brushes, layers, selections and the formats \
         the desktop reads, saved where you choose.",
    ))
    .measured_size(Scale::ONE, &theme);
    assert!(with > without, "{with} <= {without}");
    let (_, empty) = sheet(Some("")).measured_size(Scale::ONE, &theme);
    assert_eq!(empty, without, "an empty purpose states nothing");
}

#[test]
fn the_sheet_scales_with_the_density() {
    let theme = Theme::dark();
    let one = sheet(Some("Draws pictures.")).measured_size(Scale::ONE, &theme);
    let two = sheet(Some("Draws pictures."))
        .measured_size(Scale::from_percent(200).expect("in range"), &theme);
    assert!(two.0 >= one.0 * 2 - 2 && two.1 > one.1, "{one:?} {two:?}");
}

#[test]
fn every_part_draws_inside_the_sheet() {
    let theme = Theme::dark();
    let about = sheet(Some("Draws pictures."));
    let (w, h) = about.measured_size(Scale::ONE, &theme);
    let mut surface = Surface::new(w + 40, h + 40).expect("a surface");
    let blank = surface.clone();
    about.render(&mut surface, Rect::new(20, 20, w, h), Scale::ONE, &theme);
    assert_ne!(surface, blank, "it drew");
    let stride = surface.width();
    for (index, (drawn, before)) in surface.pixels().iter().zip(blank.pixels()).enumerate() {
        let index = u32::try_from(index).expect("a small surface");
        let (x, y) = (index % stride, index / stride);
        let inside = (20..20 + w).contains(&x) && (20..20 + h).contains(&y);
        assert!(
            inside || drawn == before,
            "drew at ({x}, {y}) outside the sheet"
        );
    }
}

#[test]
fn a_picture_at_the_sheets_own_side_fills_its_slot() {
    use super::ABOUT_ICON_SIDE;
    use tairix_raster::Color;
    let theme = Theme::dark();
    let mut about = sheet(None);
    let side = Scale::ONE.scale_length(ABOUT_ICON_SIDE);
    let magenta = Color::rgb(255, 0, 255).premultiply();
    about.set_artwork(Surface::filled(side, side, magenta).expect("a picture"));
    let (w, h) = about.measured_size(Scale::ONE, &theme);
    let mut surface = Surface::new(w, h).expect("a surface");
    about.render(&mut surface, Rect::new(0, 0, w, h), Scale::ONE, &theme);
    let (x, y, slot) = about.layout(w, Scale::ONE, &theme).icon;
    assert_eq!(slot, side, "the slot is the side the session rasterises at");
    for (px, py) in [(x, y), (x + side - 1, y + side - 1)] {
        assert_eq!(surface.get(px, py), Some(magenta), "({px}, {py})");
    }
}
