use alloc::string::String;
use alloc::vec::Vec;

use tairix_appconf::{Document, Keys, Live, Registry};
use tairix_colour::Rgb;
use tairix_image::IndexDepth;

use super::{GridStyle, OpenAt, PrefKey, Preferences, Shades, Surround};
use crate::pane::{PaneKind, Side};
use crate::save::SaveFormat;
use crate::tool::Tool;

fn parsed(text: &str) -> Document {
    Document::parse(text).expect("a small document")
}

/// Every setting moved off its default.
fn moved() -> Preferences {
    let mut moved = Preferences {
        tool: Tool::Eyedropper,
        open_at: OpenAt::Actual,
        format: SaveFormat::Gif,
        pixel_grid_from: 0,
        checker_side: 24,
        shades: Shades::Chosen(Rgb::new(10, 20, 30), Rgb::new(200, 210, 220)),
        surround: Surround::Chosen(Rgb::new(1, 2, 3)),
        ..Preferences::default()
    };
    moved.new.size = (320, 200);
    moved.new.depth = Some(IndexDepth::Four);
    moved.new.transparent = true;
    moved.grid.spacing = (8, 12);
    moved.grid.offset = (3, 5);
    moved.grid.colour = Rgb::new(255, 0, 128);
    moved.grid.opacity = 900;
    moved.grid.style = GridStyle::Crossings;
    moved.grid.shown = true;
    moved.grid.snap = true;
    moved.panes.move_to(PaneKind::Colour, Side::Left, 0);
    moved
}

#[test]
fn every_setting_survives_its_spelling() {
    let moved = moved();
    let document = moved.document_of(Preferences::KEYS);
    assert_eq!(Preferences::load(&document), (moved, Vec::new()));
    assert_eq!(document.get("general.tool"), Some("eyedropper"));
    assert_eq!(document.get("new.format"), Some("gif"));
    assert_eq!(document.get("canvas.checker-shades"), Some("0a141e c8d2dc"));
    assert_eq!(
        Preferences::load(&Preferences::default().document_of(Preferences::KEYS)),
        (Preferences::default(), Vec::new())
    );
}

#[test]
fn a_value_out_of_bounds_or_misspelt_costs_only_itself() {
    let (read, refused) = Preferences::load(&parsed(
        "general.tool = paintbrush\n\
         new.width = 0\n\
         new.height = 300\n\
         grid.across = 2000\n\
         grid.opacity = 10\n\
         grid.pixels-from = 150\n\
         canvas.checker-size = 1\n\
         canvas.checker-shades = 000000\n\
         panes.layout = tools:left\n",
    ));
    assert_eq!(
        refused,
        [
            PrefKey::Tool,
            PrefKey::NewWidth,
            PrefKey::GridAcross,
            PrefKey::GridOpacity,
            PrefKey::PixelGridFrom,
            PrefKey::CheckerSide,
            PrefKey::CheckerShades,
            PrefKey::Panes,
        ]
    );
    assert_eq!(read.new.size, (640, 300), "the height stands");
    assert_eq!(read.grid, Preferences::default().grid);
}

#[test]
fn colours_and_a_background_are_read_against_the_format_before_them() {
    let (gif, refused) = Preferences::load(&parsed("new.format = gif\nnew.colours = millions\n"));
    assert_eq!(
        refused,
        [PrefKey::NewColours],
        "a GIF holds no millions of colours"
    );
    assert_eq!(
        gif.new.depth,
        Some(IndexDepth::Eight),
        "and takes the most it can"
    );
    let (jpeg, refused) = Preferences::load(&parsed(
        "new.colours = 256\nnew.transparent = true\nnew.format = jpeg\n",
    ));
    assert_eq!(refused, [PrefKey::NewColours, PrefKey::NewTransparent]);
    assert_eq!((jpeg.new.depth, jpeg.new.transparent), (None, false));
}

#[test]
fn an_edit_is_told_apart_setting_by_setting() {
    let was = Preferences::default();
    let mut now = was.clone();
    now.grid.snap = true;
    now.panes.hide(PaneKind::Colour);
    assert_eq!(
        was.differing(&now),
        Keys::of(PrefKey::GridSnap).union(Keys::of(PrefKey::Panes))
    );
    let mut copy = was.clone();
    copy.set_from(&now, Keys::of(PrefKey::Panes));
    assert!(!copy.grid.snap && !copy.panes.shows(PaneKind::Colour));
}

#[test]
fn every_key_is_one_the_store_accepts() {
    let mut text = String::new();
    for &key in Preferences::KEYS {
        assert!(
            tairix_appconf::validate_key(Preferences::name(key)).is_ok(),
            "{key:?}"
        );
        text.clear();
        assert!(
            Preferences::default().spell(key, &mut text),
            "{key:?} is always stored"
        );
    }
}
