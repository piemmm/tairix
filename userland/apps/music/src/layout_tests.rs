//! Host tests for the player's window geometry.
//!
//! No test hard-codes a coordinate: each asks the layout where a band is and
//! checks a property of it, so a change to the geometry moves the tests with
//! it.

use tairix_font::BitmapFont;
use tairix_geometry::{Rect, Scale};
use tairix_theme::{TextRole, ThemeRegistry};

use super::Layout;

fn layout(width: u32, height: u32, scale: Scale) -> Layout {
    let registry = ThemeRegistry::with_builtins();
    let theme = registry.active();
    let font = BitmapFont::for_role(theme.fonts(), TextRole::Body, scale);
    Layout::for_window(width, height, theme, scale, font)
}

fn scales() -> [Scale; 3] {
    [
        Scale::ONE,
        Scale::from_percent(150).expect("a legal scale"),
        Scale::from_percent(200).expect("a legal scale"),
    ]
}

fn overlaps(a: Rect, b: Rect) -> bool {
    !a.intersection(&b).is_empty()
}

fn controls(layout: &Layout) -> [(&'static str, Rect); 17] {
    [
        ("art", layout.art()),
        ("title", layout.title()),
        ("subtitle", layout.subtitle()),
        ("format", layout.format()),
        ("elapsed", layout.elapsed()),
        ("seek", layout.seek()),
        ("total", layout.total()),
        ("meters", layout.meters()),
        ("previous", layout.previous()),
        ("play", layout.play()),
        ("next", layout.next()),
        ("shuffle", layout.shuffle()),
        ("repeat", layout.repeat()),
        ("volume icon", layout.volume_icon()),
        ("volume", layout.volume()),
        ("rows", layout.rows()),
        ("status", layout.status()),
    ]
}

#[test]
fn every_part_has_room_lies_inside_the_window_and_overlaps_no_other_at_every_scale() {
    for scale in scales() {
        let (width, height) = (scale.scale_length(720), scale.scale_length(520));
        let layout = layout(width, height, scale);
        assert_eq!(layout.window(), Rect::new(0, 0, width, height));
        let parts = controls(&layout);
        for (name, rect) in parts {
            assert!(!rect.is_empty(), "{name} has room at {scale:?}");
            assert_eq!(
                rect.intersection(&layout.window()),
                rect,
                "{name} lies inside the window"
            );
        }
        for (i, (a, ra)) in parts.iter().enumerate() {
            for (b, rb) in &parts[i + 1..] {
                assert!(!overlaps(*ra, *rb), "{a} and {b} overlap at {scale:?}");
            }
        }
        assert_eq!(layout.art().width, layout.art().height, "the art is square");
        assert!(layout.header().bottom() <= layout.rows().top());
        assert!(layout.rows().bottom() <= layout.status().top());
        assert_eq!(layout.scrollbar().top(), layout.rows().top());
    }
}

#[test]
fn the_transport_keeps_its_room_and_the_playlist_gives_way_as_the_window_shrinks() {
    let tall = layout(720, 520, Scale::ONE);
    let short = layout(720, 260, Scale::ONE);
    for (name, a, b) in [
        ("play", tall.play(), short.play()),
        ("seek", tall.seek(), short.seek()),
        ("volume", tall.volume(), short.volume()),
    ] {
        assert_eq!(a, b, "{name} does not move");
    }
    assert!(short.rows().height < tall.rows().height);
    for tiny in [layout(0, 0, Scale::ONE), layout(40, 30, Scale::ONE)] {
        for (name, rect) in controls(&tiny) {
            assert_eq!(
                rect.intersection(&tiny.window()),
                rect,
                "{name} stays inside a tiny window"
            );
        }
    }
}

#[test]
fn the_columns_fill_the_rows_and_rows_are_found_where_they_are_drawn() {
    let layout = layout(720, 520, Scale::ONE);
    let columns: u32 = layout.columns().iter().sum();
    assert!(columns <= layout.rows().width);
    assert!(
        layout.columns()[1] >= layout.columns()[2],
        "the title takes more"
    );
    let pitch = layout.row_pitch();
    let scroll = pitch / 2;
    for index in 0..4 {
        let row = layout.row(index, scroll);
        if row.is_empty() {
            continue;
        }
        let middle = row.top() + i32::try_from(row.height / 2).expect("small");
        assert_eq!(layout.row_at(middle, scroll), Some(index));
    }
    assert_eq!(layout.row_at(layout.rows().top() - 1, 0), None);
    assert_eq!(layout.row_at(layout.rows().bottom(), 0), None);
    assert!(
        layout.row(10_000, 0).is_empty(),
        "a row out of sight has no rectangle"
    );
}

#[test]
fn the_rows_in_sight_are_exactly_those_with_a_rectangle() {
    let window = layout(720, 520, Scale::ONE);
    let pitch = window.row_pitch();
    for scroll in [0, 1, pitch / 2, pitch, pitch * 7 + 3] {
        let visible = window.visible_rows(scroll);
        assert!(!visible.is_empty());
        for index in 0..visible.end + 4 {
            assert_eq!(
                visible.contains(&index),
                !window.row(index, scroll).is_empty(),
                "row {index}, scrolled {scroll}"
            );
        }
    }
    let tiny = layout(0, 0, Scale::ONE);
    assert!(tiny.visible_rows(0).is_empty(), "no room shows no row");
}
