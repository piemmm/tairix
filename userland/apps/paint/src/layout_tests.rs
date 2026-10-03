use tairix_geometry::Scale;
use tairix_theme::{Theme, ThemeRegistry};

use super::{Faces, Floor, Layout, Needs, LEAST_WELL, MIN_CANVAS, MOST_WELL};
use crate::view::MOST_WELLS;

const SLOT: u32 = 28;

fn needs(wells: usize) -> Needs {
    Needs {
        wells,
        picker: 300,
        tool_box: SLOT,
        view_strip: 188,
        bar_rows: 1,
    }
}

fn layout(theme: &Theme, width: u32, height: u32, needs: Needs) -> Layout {
    Layout::for_window(
        width,
        height,
        theme,
        Scale::ONE,
        Faces::of(theme, Scale::ONE),
        needs,
    )
}

#[test]
fn the_bands_tile_the_window_without_overlapping() {
    let registry = ThemeRegistry::with_builtins();
    let layout = layout(registry.active(), 900, 640, needs(17));
    let window = layout.window();
    for band in [
        layout.top(),
        layout.controls(),
        layout.view_strip(),
        layout.tool_box(),
        layout.dock(),
        layout.canvas(),
        layout.vertical_bar(),
        layout.horizontal_bar(),
        layout.palette(),
        layout.status(),
    ] {
        assert!(!band.is_empty());
        assert_eq!(band.intersection(&window), band, "inside the window");
    }
    for chrome in [
        layout.top(),
        layout.tool_box(),
        layout.dock(),
        layout.palette(),
        layout.status(),
    ] {
        assert!(layout.canvas().intersection(&chrome).is_empty());
    }
    assert_eq!(
        layout.tools().width,
        SLOT,
        "the tool box as broad as its tools"
    );
    assert_eq!(layout.tool_box().right(), layout.canvas().left());
    assert_eq!(layout.canvas().right(), layout.vertical_bar().left());
    assert_eq!(
        layout.vertical_bar().right(),
        layout.dock().left(),
        "the dock on the right"
    );
    assert_eq!(layout.horizontal_bar().bottom(), layout.palette().top());
    assert_eq!(
        (layout.palette().left(), layout.palette().right()),
        (layout.tool_box().right(), layout.dock().left()),
        "the palette strip runs between the tool box and the dock"
    );
    assert_eq!(layout.palette().bottom(), layout.status().top());
    assert_eq!(layout.view_strip().width, 188);
    assert!(layout.controls().right() < layout.view_strip().left());
    assert_eq!(layout.view_strip().right(), layout.top().right() - 8);
    assert_eq!(layout.picker().height, 300);
    assert!(
        layout.picker().top() > layout.wells().bottom(),
        "the picker under the wells"
    );
    assert_eq!(
        layout.primary_well().intersection(&layout.wells()),
        layout.primary_well()
    );
}

#[test]
fn a_few_colours_are_one_row_of_broad_wells() {
    let registry = ThemeRegistry::with_builtins();
    let layout = layout(registry.active(), 900, 640, needs(17));
    assert_eq!(layout.columns(), 17);
    assert_eq!(layout.swatches().height, MOST_WELL);
    assert_eq!(layout.swatches().width, 17 * MOST_WELL);
    assert_eq!(
        layout.swatches().intersection(&layout.palette()),
        layout.swatches()
    );
}

#[test]
fn a_full_palette_takes_as_few_rows_as_hold_it() {
    let registry = ThemeRegistry::with_builtins();
    let layout = layout(registry.active(), 900, 640, needs(MOST_WELLS));
    let swatches = layout.swatches();
    let columns = u32::try_from(layout.columns()).expect("a count");
    let side = swatches.width / columns;
    let rows = swatches.height / side;
    assert!(side >= LEAST_WELL, "every well can be aimed at");
    assert!(columns * rows >= 257, "every entry has a well");
    assert!(columns * (rows - 1) < 257, "and no row is spare");
    assert!(swatches.width <= layout.palette().width - 16);
    assert!(
        rows > 1,
        "a 256-colour palette does not fit one row of a 900-wide window"
    );
}

#[test]
fn the_rows_follow_the_width() {
    let registry = ThemeRegistry::with_builtins();
    let narrow = layout(registry.active(), 700, 900, needs(MOST_WELLS));
    let wide = layout(registry.active(), 1600, 900, needs(MOST_WELLS));
    assert!(wide.columns() > narrow.columns());
    assert!(wide.palette().height < narrow.palette().height);
}

fn floor() -> Floor {
    Floor {
        controls: 520,
        view_strip: 188,
        tool_box: (SLOT, 3 * SLOT + 8),
        wells: MOST_WELLS,
    }
}

#[test]
fn the_least_window_seats_its_bars_and_the_largest_palette_round_a_canvas() {
    let registry = ThemeRegistry::with_builtins();
    let theme = registry.active();
    let floor = floor();
    let mut asked = None;
    let (width, height) = Layout::min_size(
        theme,
        Scale::ONE,
        Faces::of(theme, Scale::ONE),
        floor,
        |across| {
            asked = Some(across);
            3
        },
    );
    let least = layout(
        theme,
        width,
        height,
        Needs {
            bar_rows: 3,
            ..needs(MOST_WELLS)
        },
    );
    assert_eq!(
        asked,
        Some(least.controls().width),
        "asked for the rows across the bar it lays out"
    );
    assert!(
        least.controls().width >= floor.controls,
        "every setting of every tool's bar seated"
    );
    assert_eq!(
        least.controls().height,
        28 * 3 + 8 * 2,
        "three rows a gap apart"
    );
    assert!(least.canvas().width >= MIN_CANVAS);
    assert!(least.canvas().height >= MIN_CANVAS);
    assert!(least.tools().height >= floor.tool_box.1, "a tool showing");
    assert_eq!(
        least.dock().width,
        Layout::dock_inner_width(theme, Scale::ONE) + 16,
        "the dock at its width"
    );
}

#[test]
fn a_tiny_window_gives_up_its_canvas_first() {
    let registry = ThemeRegistry::with_builtins();
    let theme = registry.active();
    let squeezed = layout(theme, 40, 30, needs(17));
    assert!(squeezed.canvas().width < 40);
    for band in [
        squeezed.top(),
        squeezed.tool_box(),
        squeezed.dock(),
        squeezed.canvas(),
        squeezed.palette(),
        squeezed.status(),
    ] {
        assert!(
            band.is_empty() || band.intersection(&squeezed.window()) == band,
            "nothing laid out past the window"
        );
    }
}

#[test]
fn a_dock_asking_for_more_than_there_is_is_cut_to_the_dock() {
    let registry = ThemeRegistry::with_builtins();
    let layout = layout(
        registry.active(),
        900,
        640,
        Needs {
            picker: 10_000,
            ..needs(17)
        },
    );
    assert!(layout.picker().bottom() <= layout.dock().bottom());
}

#[test]
fn the_view_strip_keeps_the_first_row_and_the_bar_its_column_below() {
    let registry = ThemeRegistry::with_builtins();
    let theme = registry.active();
    let two = layout(
        theme,
        900,
        640,
        Needs {
            bar_rows: 2,
            ..needs(16)
        },
    );
    let one = layout(theme, 900, 640, needs(16));
    assert_eq!(two.view_strip().height, one.view_strip().height, "one row");
    assert_eq!(two.view_strip().top(), two.controls().top());
    assert!(two.controls().right() < two.view_strip().left());
    assert!(two.top().height > one.top().height);
    assert_eq!(
        two.canvas().top() - one.canvas().top(),
        i32::try_from(two.top().height - one.top().height).expect("small")
    );
    assert_eq!(
        Layout::controls_width(900, 188, theme, Scale::ONE),
        one.controls().width
    );
}
