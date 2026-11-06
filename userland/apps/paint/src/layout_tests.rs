use tairix_abi::window_ipc::WindowSizing;
use tairix_controls::{ScrollBar, ScrollOrientation};
use tairix_geometry::Scale;
use tairix_theme::{Theme, ThemeRegistry};

use super::{Faces, Floor, Layout, Needs, LEAST_WELL, MIN_CANVAS, MOST_WELL, WELLS_HEIGHT};
use crate::layers_pane::LayerMeasures;
use crate::pane::{Arrangement, PaneKind, Side};
use crate::view::MOST_WELLS;

const SLOT: u32 = 24;

fn needs(wells: usize) -> Needs {
    Needs {
        wells,
        clear: false,
        picker: 300,
        colour_controls: 0,
        recents: 0,
        tool_box: SLOT,
        tool_box_length: 9 * (SLOT + 8),
        tool_box_least: 3 * SLOT + 8,
        layers: LayerMeasures {
            panel: 80,
            row: 34,
            count: 3,
        },
        operations: [super::BodyHeights::default(); 2],
        view_strip: 188,
        bar_rows: 1,
        chosen: [None; PaneKind::ALL.len()],
    }
}

fn layout(theme: &Theme, width: u32, height: u32, needs: Needs) -> Layout {
    arranged(theme, width, height, needs, &Arrangement::default())
}

fn arranged(theme: &Theme, width: u32, height: u32, needs: Needs, panes: &Arrangement) -> Layout {
    Layout::for_window(
        width,
        height,
        theme,
        Scale::ONE,
        Faces::of(theme, Scale::ONE),
        (needs, panes),
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
    let (left, right) = (
        layout.dock_on(Side::Left).rect,
        layout.dock_on(Side::Right).rect,
    );
    assert_eq!(left.right(), layout.canvas().left());
    assert_eq!(layout.canvas().right(), layout.vertical_bar().left());
    assert_eq!(
        layout.vertical_bar().right(),
        right.left(),
        "the right dock beside the bar"
    );
    for (dock, body) in [(left, layout.tool_box()), (right, layout.dock())] {
        assert_eq!(dock.intersection(&body), body, "each pane inside its dock");
    }
    assert_eq!(layout.horizontal_bar().bottom(), layout.palette().top());
    assert_eq!(
        (layout.palette().left(), layout.palette().right()),
        (left.right(), right.left()),
        "the palette strip runs between the docks"
    );
    assert_eq!(layout.palette().bottom(), layout.status().top());
    assert_eq!(layout.view_strip().width, 188);
    assert!(layout.controls().right() < layout.view_strip().left());
    let gap = tairix_geometry::to_i32(registry.active().metrics().control_gap);
    assert_eq!(layout.view_strip().right(), layout.top().right() - gap);
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
fn a_full_palette_is_one_row_that_scrolls_where_it_overflows() {
    let registry = ThemeRegistry::with_builtins();
    let layout = layout(registry.active(), 900, 640, needs(MOST_WELLS));
    let (row, shown) = (layout.swatches(), layout.wells_view());
    let side = row.height;
    assert!(side >= LEAST_WELL, "every well can be aimed at");
    assert_eq!(row.width, side * 257, "every entry has a well, in one row");
    assert!(
        row.width > shown.width,
        "a 256-colour palette overflows 900 pixels"
    );
    assert!(!layout.palette_bar().is_empty(), "so the strip scrolls");
    assert_eq!(layout.palette_bar().top(), shown.bottom());
}

#[test]
fn a_short_palette_shows_whole_and_needs_no_bar() {
    let registry = ThemeRegistry::with_builtins();
    let layout = layout(registry.active(), 900, 640, needs(17));
    assert!(layout.palette_bar().is_empty());
    assert_eq!(layout.swatches().width, layout.wells_view().width);
    assert!(layout.swatches().height <= MOST_WELL);
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
    let metrics = theme.metrics();
    assert_eq!(
        least.controls().height,
        metrics.control_height * 3 + metrics.control_gap * 2,
        "three rows a gap apart"
    );
    assert!(least.canvas().width >= MIN_CANVAS);
    assert!(least.canvas().height >= MIN_CANVAS);
    assert!(least.tools().height >= floor.tool_box.1, "a tool showing");
    assert!(
        !slot(&least, PaneKind::Colour).bar.is_empty(),
        "held to its least, the colour pane scrolls"
    );
    assert_eq!(
        least.dock_on(Side::Right).rect.width,
        Layout::dock_inner_width(theme, Scale::ONE)
            + 2 * metrics.control_gap
            + metrics.scrollbar_breadth,
        "the dock at its width beside the bar"
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

/// A body asking for more than its dock holds stays within the dock and is
/// laid out whole beside a bar that scrolls it, the dock widened by the bar
/// so its content keeps its width.
#[test]
fn a_body_asking_for_more_than_its_dock_holds_scrolls_within_it() {
    let registry = ThemeRegistry::with_builtins();
    let theme = registry.active();
    let layout = layout(
        theme,
        900,
        640,
        Needs {
            picker: 10_000,
            ..needs(17)
        },
    );
    let colour = slot(&layout, PaneKind::Colour);
    let dock = layout.dock_on(Side::Right).rect;
    assert!(
        colour.body.bottom() <= dock.bottom(),
        "the body stays in its dock"
    );
    assert!(!colour.bar.is_empty(), "and scrolls by a bar");
    assert_eq!(
        (colour.bar.right(), colour.bar.height),
        (colour.body.right(), colour.body.height),
        "down its right"
    );
    assert_eq!(colour.viewport().right(), colour.bar.left());
    assert_eq!(colour.content.top(), colour.body.top());
    assert!(colour.content.height > colour.body.height, "laid out whole");
    assert_eq!(layout.picker().height, 10_000, "the picker kept whole");
    let metrics = theme.metrics();
    assert_eq!(
        dock.width,
        Layout::dock_inner_width(theme, Scale::ONE)
            + 2 * metrics.control_gap
            + metrics.scrollbar_breadth,
        "the dock widened by the bar"
    );
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

/// The header a pane is laid out with: its band atop its plate, and its body
/// beneath.
fn slot(layout: &Layout, kind: PaneKind) -> super::PaneSlot {
    *layout.pane(kind).expect("the pane is shown")
}

/// An operation pane given less than shows it whole has its settings laid out
/// whole from its body's top beside the bar, as the Colour pane's are, rather
/// than squeezed into what shows.
#[test]
fn an_operation_pane_given_less_lays_its_settings_out_whole() {
    let registry = ThemeRegistry::with_builtins();
    let theme = registry.active();
    let mut needs = needs(17);
    needs.operations[0] = super::BodyHeights {
        least: 40,
        natural: 5_000,
        fit: super::Fit::Scrolled,
    };
    let mut panes = Arrangement::default();
    panes.show(PaneKind::Adjustments);
    let layout = arranged(theme, 900, 640, needs, &panes);
    let adjustments = slot(&layout, PaneKind::Adjustments);
    assert!(!adjustments.bar.is_empty(), "scrolled by a bar");
    let gap = Scale::ONE.scale_length(theme.metrics().control_gap).max(1);
    let settings = layout.operation_settings(PaneKind::Adjustments);
    assert_eq!(settings, adjustments.content.inset(gap / 2));
    assert!(settings.height > adjustments.body.height, "laid out whole");
    assert!(settings.right() <= adjustments.bar.left(), "beside the bar");
}

#[test]
fn a_pane_is_its_mini_band_over_its_body() {
    let registry = ThemeRegistry::with_builtins();
    let theme = registry.active();
    let layout = layout(theme, 900, 640, needs(17));
    let band = tairix_controls::TitleBar::height_of(
        tairix_controls::TitleBarCommands::Pane,
        Scale::ONE,
        theme,
    );
    for kind in [PaneKind::Tools, PaneKind::Colour] {
        let pane = slot(&layout, kind);
        assert_eq!(pane.header.top(), pane.frame.top(), "{kind:?}");
        assert_eq!(pane.header.height, band, "{kind:?}");
        assert_eq!(pane.body.top(), pane.header.bottom(), "{kind:?}");
        assert_eq!(pane.body.bottom(), pane.frame.bottom(), "{kind:?}");
    }
    assert_eq!(layout.pane(PaneKind::Adjustments), None);
    assert!(layout.operation(PaneKind::Adjustments).is_empty());
    assert!(layout.layers().rows.is_empty(), "the Layers pane hidden");
    assert_eq!(
        slot(&layout, PaneKind::Colour)
            .body
            .intersection(&layout.picker()),
        layout.picker()
    );
}

#[test]
fn a_rolled_up_pane_shows_its_band_alone_and_a_hidden_one_is_nowhere() {
    let registry = ThemeRegistry::with_builtins();
    let theme = registry.active();
    let mut panes = Arrangement::default();
    panes.toggle_collapsed(PaneKind::Colour);
    let rolled = arranged(theme, 900, 640, needs(17), &panes);
    let colour = slot(&rolled, PaneKind::Colour);
    assert_eq!(colour.frame, colour.header);
    assert!(colour.body.is_empty());
    assert!(rolled.picker().is_empty() && rolled.wells().is_empty());
    panes.hide(PaneKind::Tools);
    let hidden = arranged(theme, 900, 640, needs(17), &panes);
    assert_eq!(hidden.pane(PaneKind::Tools), None);
    assert!(hidden.tools().is_empty());
    assert!(
        hidden.dock_on(Side::Left).rect.is_empty(),
        "an empty dock takes no width"
    );
    assert!(
        hidden.canvas().width > layout(theme, 900, 640, needs(17)).canvas().width,
        "the canvas takes the room"
    );
}

#[test]
fn panes_stack_down_a_dock_and_one_past_the_room_shows_its_band() {
    let registry = ThemeRegistry::with_builtins();
    let theme = registry.active();
    let mut panes = Arrangement::default();
    panes.move_to(PaneKind::Colour, Side::Left, 1);
    let both = arranged(theme, 900, 640, needs(17), &panes);
    let (tools, colour) = (slot(&both, PaneKind::Tools), slot(&both, PaneKind::Colour));
    assert!(
        colour.frame.top() > tools.frame.bottom(),
        "the colour pane beneath the tools"
    );
    assert_eq!(
        both.dock_on(Side::Left).rect.width,
        Layout::dock_inner_width(theme, Scale::ONE) + 2 * theme.metrics().control_gap,
        "the dock as broad as its broadest pane"
    );
    assert!(both.dock_on(Side::Right).rect.is_empty());
    // A window too short for both bodies gives the lower pane its band.
    let short = arranged(
        theme,
        900,
        420,
        Needs {
            tool_box_length: 300,
            ..needs(17)
        },
        &panes,
    );
    let colour = slot(&short, PaneKind::Colour);
    assert!(colour.header.height > 0, "its band is always seated");
    assert!(colour.frame.bottom() <= short.dock_on(Side::Left).rect.bottom());
}

#[test]
fn the_colour_pane_stacks_its_wells_controls_picker_and_recent_colours() {
    let registry = ThemeRegistry::with_builtins();
    let mut tall = needs(17);
    tall.picker = 200;
    tall.colour_controls = 96;
    tall.recents = 54;
    let layout = layout(registry.active(), 900, 900, tall);
    assert!(layout.wells().bottom() < layout.colour_controls().top());
    assert!(layout.colour_controls().bottom() < layout.picker().top());
    assert!(layout.picker().bottom() < layout.recents().top());
    assert_eq!(layout.recents().height, 54);
    let short = layout_short(registry.active(), tall);
    assert_eq!(short.recents().height, 54, "every part kept whole");
    assert!(short.picker().bottom() < short.recents().top());
    assert!(
        !slot(&short, PaneKind::Colour).bar.is_empty(),
        "scrolled into view by the pane's bar"
    );
}

fn layout_short(theme: &Theme, needs: Needs) -> Layout {
    layout(theme, 900, 440, needs)
}

#[test]
fn the_strip_seats_its_pop_out_at_its_right_end() {
    let registry = ThemeRegistry::with_builtins();
    let theme = registry.active();
    let gap = tairix_geometry::to_i32(theme.metrics().control_gap);
    for wells in [17, MOST_WELLS] {
        let layout = layout(theme, 900, 640, needs(wells));
        let (strip, pop_out) = (layout.palette(), layout.pop_out());
        assert_eq!(pop_out.right(), strip.right() - gap, "{wells}: at its end");
        assert_eq!(
            (pop_out.top(), pop_out.bottom()),
            (strip.top() + gap, strip.bottom() - gap),
            "{wells}: as tall as its inside"
        );
        assert_eq!(pop_out.width, pop_out.height, "{wells}: square");
        for beside in [layout.wells_view(), layout.palette_bar()] {
            assert!(
                beside.is_empty() || beside.right() < pop_out.left(),
                "{wells}: clear of the wells and their bar"
            );
        }
    }
}

#[test]
fn a_floating_palette_is_a_near_square_grid_beside_the_window() {
    let registry = ThemeRegistry::with_builtins();
    let theme = registry.active();
    let mut panes = Arrangement::default();
    panes.float(PaneKind::Palette);
    let colours = Needs {
        clear: true,
        ..needs(17)
    };
    let floating = arranged(theme, 900, 640, colours, &panes);
    assert_eq!(floating.columns(), 4, "sixteen colours four across");
    let wells = floating.swatches();
    assert_eq!(
        (wells.width, wells.height),
        (4 * MOST_WELL, 5 * MOST_WELL),
        "the clear ink on a row of its own"
    );
    assert_eq!(floating.wells_view(), wells, "it opens showing every well");
    assert!(floating.palette_bar().is_empty() && floating.pop_out().is_empty());
    let frame = floating.pane(PaneKind::Palette).expect("floating").frame;
    assert_eq!(floating.palette(), frame);
    assert_eq!(wells.intersection(&frame), wells);
    assert!(
        frame.intersection(&floating.window()).is_empty(),
        "beside the window"
    );
    let docked = layout(theme, 900, 640, colours);
    assert!(
        floating.canvas().height > docked.canvas().height,
        "the strip's room is the canvas's"
    );
}

#[test]
fn a_full_palette_floats_sixteen_across() {
    let registry = ThemeRegistry::with_builtins();
    let mut panes = Arrangement::default();
    panes.float(PaneKind::Palette);
    for (clear, rows) in [(false, 16), (true, 17)] {
        let full = Needs {
            clear,
            ..needs(256 + usize::from(clear))
        };
        let floating = arranged(registry.active(), 900, 640, full, &panes);
        assert_eq!(floating.columns(), 16, "masked: {clear}");
        assert_eq!(
            floating.swatches().height,
            rows * MOST_WELL,
            "masked: {clear}"
        );
    }
}

#[test]
fn a_hidden_palette_claims_no_strip() {
    let registry = ThemeRegistry::with_builtins();
    let mut panes = Arrangement::default();
    panes.hide(PaneKind::Palette);
    let hidden = arranged(registry.active(), 900, 640, needs(17), &panes);
    assert!(hidden.palette().is_empty() && hidden.wells_view().is_empty());
    assert!(hidden.pop_out().is_empty());
    assert_eq!(hidden.pane(PaneKind::Palette), None);
    assert_eq!(hidden.horizontal_bar().bottom(), hidden.status().top());
}

#[test]
fn the_least_window_lays_out_the_strip_it_was_floored_on() {
    let registry = ThemeRegistry::with_builtins();
    let theme = registry.active();
    let (width, height) = Layout::min_size(
        theme,
        Scale::ONE,
        Faces::of(theme, Scale::ONE),
        floor(),
        |_| 1,
    );
    let least = layout(theme, width, height, needs(MOST_WELLS));
    assert!(!least.pop_out().is_empty(), "the strip seats its pop-out");
    assert_eq!(
        least.canvas().height,
        MIN_CANVAS,
        "the strip is exactly as tall as the floor allowed it"
    );
}

/// The layout of a palette of `wells` wells, the last the clear ink, floating
/// in a tool window resized to `size`.
fn floating_at(theme: &Theme, wells: usize, size: (u32, u32)) -> Layout {
    let mut panes = Arrangement::default();
    panes.float(PaneKind::Palette);
    let mut chosen = [None; PaneKind::ALL.len()];
    chosen[PaneKind::Palette.index()] = Some(size);
    let colours = Needs {
        clear: true,
        chosen,
        ..needs(wells)
    };
    arranged(theme, 900, 640, colours, &panes)
}

/// The client length `count` wells span floating, with the gap either side.
fn wells_of(theme: &Theme, count: u32) -> u32 {
    count * MOST_WELL + 2 * theme.metrics().control_gap
}

/// A floating palette keeps its wells' size and sets as many to a row as the
/// width it is given holds, from one up to every well.
#[test]
fn a_floating_palette_reflows_to_the_width_it_is_given() {
    let registry = ThemeRegistry::with_builtins();
    let theme = registry.active();
    for (across, columns) in [(0, 1), (1, 1), (3, 3), (6, 6), (17, 17), (40, 17)] {
        let palette = floating_at(theme, 17, (wells_of(theme, across), wells_of(theme, 17)));
        assert_eq!(palette.columns(), columns, "{across} wells across");
        assert_eq!(palette.palette_well(), MOST_WELL, "{across} wells across");
        assert_eq!(
            palette.swatches().width,
            MOST_WELL * u32::try_from(columns).expect("fits"),
            "{across} wells across"
        );
    }
}

/// A floating palette scrolls down, by a bar down its right, only where the
/// rows its width sets out are more than its height holds; the strip scrolls
/// along.
#[test]
fn a_floating_palette_scrolls_down_only_where_its_wells_overflow() {
    let registry = ThemeRegistry::with_builtins();
    let theme = registry.active();
    let metrics = theme.metrics();
    let (gap, bar) = (metrics.control_gap, metrics.scrollbar_breadth);
    let fits = floating_at(theme, 17, (wells_of(theme, 6), wells_of(theme, 3)));
    assert_eq!(fits.columns(), 6);
    assert!(
        fits.palette_bar().is_empty(),
        "three rows of six hold seventeen"
    );
    assert_eq!(fits.wells_view(), fits.swatches());
    let short = floating_at(theme, 17, (wells_of(theme, 6), wells_of(theme, 2) + 8));
    assert_eq!(short.columns(), 5, "the bar takes a column's room");
    assert_eq!(short.palette_axis(), ScrollOrientation::Vertical);
    let (room, scroll) = (short.palette().inset(gap), short.palette_bar());
    assert_eq!(
        (scroll.right(), scroll.top(), scroll.width, scroll.height),
        (room.right(), room.top(), bar, room.height),
        "down the room's right"
    );
    assert_eq!(
        short.swatches().height,
        4 * MOST_WELL,
        "seventeen in rows of five"
    );
    assert_eq!(short.wells_view().height, room.height);
    assert!(
        short.wells_view().right() <= scroll.left(),
        "clear of the bar"
    );
    let strip = layout(theme, 900, 640, needs(17));
    assert_eq!(strip.palette_axis(), ScrollOrientation::Horizontal);
}

/// A floating palette may be resized between one column of its wells, with
/// its bar where it may need one, and one row of them; it opens inside that
/// range and never shows a bar shorter than the least one is drawn at.
#[test]
fn a_floating_palette_is_held_between_one_column_and_one_row() {
    let registry = ThemeRegistry::with_builtins();
    let theme = registry.active();
    let metrics = theme.metrics();
    let (gap, bar) = (metrics.control_gap, metrics.scrollbar_breadth);
    let least = ScrollBar::least_length(bar, Scale::ONE, theme);
    for (wells, inner) in [
        (2, (2 * MOST_WELL, MOST_WELL)),
        (5, (3 * MOST_WELL, 2 * MOST_WELL)),
        (17, (MOST_WELL + bar, least)),
        (257, (MOST_WELL + bar, least)),
    ] {
        let mut panes = Arrangement::default();
        panes.float(PaneKind::Palette);
        let colours = Needs {
            clear: true,
            ..needs(wells)
        };
        let opened = arranged(theme, 900, 640, colours, &panes);
        let palette = opened.pane(PaneKind::Palette).expect("floating");
        let (least_w, least_h) = (inner.0 + 2 * gap, inner.1 + 2 * gap);
        let line = u32::try_from(wells).expect("fits") * MOST_WELL + 2 * gap;
        assert_eq!(
            palette.sizing,
            WindowSizing::Resizable {
                min_width_px: least_w,
                min_height_px: least_h,
                max_width_px: line,
                max_height_px: line,
            },
            "{wells} wells"
        );
        assert!(
            (least_w..=line).contains(&palette.frame.width)
                && (least_h..=line).contains(&palette.frame.height),
            "{wells} wells open inside their range"
        );
        let held = floating_at(theme, wells, (u32::MAX, 1));
        let frame = held.pane(PaneKind::Palette).expect("floating").frame;
        assert_eq!(
            (frame.width, frame.height),
            (line, least_h),
            "{wells} wells held to their range"
        );
        let shown = floating_at(theme, wells, (0, 0)).palette_bar();
        assert!(
            shown.is_empty() || shown.height >= least,
            "{wells} wells: no bar shorter than its least"
        );
    }
}

/// Only the floating palette and the floating Layers pane may be resized;
/// every other floating pane, and every docked one, keeps its own size.
#[test]
fn only_the_floating_palette_and_layers_are_resizable() {
    let registry = ThemeRegistry::with_builtins();
    let mut panes = Arrangement::default();
    for kind in [PaneKind::Palette, PaneKind::Tools, PaneKind::Layers] {
        panes.float(kind);
    }
    let floating = arranged(registry.active(), 900, 640, needs(17), &panes);
    assert_eq!(floating.floating().len(), 3);
    for slot in floating.floating() {
        assert_eq!(
            slot.sizing.resizable(),
            slot.kind != PaneKind::Tools,
            "{:?}",
            slot.kind
        );
    }
    let docked = layout(registry.active(), 900, 640, needs(17));
    for slot in docked.panes() {
        assert_eq!(slot.sizing, WindowSizing::Fixed, "{:?}", slot.kind);
    }
}

/// A palette of fewer wells than four opens in one row of them, no broader
/// than its wells.
#[test]
fn a_palette_of_fewer_wells_than_four_floats_in_one_row_of_them() {
    let registry = ThemeRegistry::with_builtins();
    let theme = registry.active();
    let mut panes = Arrangement::default();
    panes.float(PaneKind::Palette);
    let colours = Needs {
        clear: true,
        ..needs(3)
    };
    let tiny = arranged(theme, 900, 640, colours, &panes);
    assert_eq!(tiny.columns(), 3);
    let frame = tiny.pane(PaneKind::Palette).expect("floating").frame;
    let gap = theme.metrics().control_gap;
    assert_eq!(
        (frame.width, frame.height),
        (3 * MOST_WELL + 2 * gap, MOST_WELL + 2 * gap)
    );
}

/// What the Colour and Layers panes ask for in the two tests below: a
/// colour picker far taller than any dock, and forty rows of layers.
fn crowded() -> Needs {
    Needs {
        picker: 2000,
        colour_controls: 60,
        layers: LayerMeasures {
            panel: 80,
            row: 34,
            count: 40,
        },
        ..needs(17)
    }
}

/// A dock seats every band first, then each body its least, then grows the
/// bodies towards what shows them whole, the pane opened last first.
#[test]
fn a_dock_seats_every_band_then_each_least_then_grows_the_pane_opened_last() {
    let registry = ThemeRegistry::with_builtins();
    let theme = registry.active();
    let gap = theme.metrics().control_gap;
    let band = tairix_controls::TitleBar::height_of(
        tairix_controls::TitleBarCommands::Pane,
        Scale::ONE,
        theme,
    );
    let colour_least = WELLS_HEIGHT + 60 + 2 * gap;
    let layers_least = 80 + gap + 2 * 34 + gap;
    let mut panes = Arrangement::default();
    panes.show(PaneKind::Layers);
    let opened_layers_last = arranged(theme, 900, 700, crowded(), &panes);
    let dock = opened_layers_last.dock_on(Side::Right).rect;
    let (colour, layers) = (
        slot(&opened_layers_last, PaneKind::Colour),
        slot(&opened_layers_last, PaneKind::Layers),
    );
    assert_eq!((colour.header.height, layers.header.height), (band, band));
    assert_eq!(
        colour.body.height, colour_least,
        "the colour pane at its least"
    );
    assert_eq!(
        layers.frame.bottom(),
        dock.bottom() - tairix_geometry::to_i32(gap / 2),
        "the Layers pane, opened last, given the rest"
    );
    panes.toggle_collapsed(PaneKind::Colour);
    panes.toggle_collapsed(PaneKind::Colour);
    let opened_colour_last = arranged(theme, 900, 700, crowded(), &panes);
    assert_eq!(
        slot(&opened_colour_last, PaneKind::Layers).body.height,
        layers_least,
        "now the Layers pane keeps its least"
    );
    assert!(
        slot(&opened_colour_last, PaneKind::Colour).body.height > colour_least,
        "and the colour pane, opened last, grows"
    );
    let tiny = arranged(theme, 900, 140, crowded(), &panes);
    for kind in [PaneKind::Colour, PaneKind::Layers] {
        assert_eq!(
            slot(&tiny, kind).header.height,
            band,
            "{kind:?}: its band seated"
        );
    }
}

/// The Layers pane scrolls its own rows: given less than every row, it is
/// laid out in what it is given, the bar its own.
#[test]
fn the_layers_pane_scrolls_its_own_rows_beneath_its_controls() {
    let registry = ThemeRegistry::with_builtins();
    let theme = registry.active();
    let mut panes = Arrangement::default();
    panes.show(PaneKind::Layers);
    let short = arranged(theme, 900, 700, crowded(), &panes);
    let layers = slot(&short, PaneKind::Layers);
    assert!(layers.bar.is_empty(), "no bar of the dock's");
    assert_eq!(layers.content, layers.body);
    let placed = short.layers();
    assert_eq!(placed.panel.height, 80, "the controls stay put");
    assert!(placed.rows.top() > placed.panel.bottom());
    assert!(!placed.bar.is_empty(), "forty rows overflow");
    assert_eq!(
        placed.bar.right(),
        placed.rows.right() + tairix_geometry::to_i32(placed.bar.width)
    );
    assert_eq!(placed.count, 40);
}

/// The Layers pane floats in a window resized between its least and the
/// height that shows every row, and from its breadth to twice it.
#[test]
fn a_floating_layers_pane_is_resized_between_its_least_and_every_row() {
    let registry = ThemeRegistry::with_builtins();
    let theme = registry.active();
    let gap = theme.metrics().control_gap;
    let mut panes = Arrangement::default();
    panes.float(PaneKind::Layers);
    let opened = arranged(theme, 900, 640, needs(17), &panes);
    let width = Layout::dock_inner_width(theme, Scale::ONE) + 2 * gap;
    let (least, natural) = (80 + gap + 2 * 34 + gap, 80 + gap + 3 * 34 + gap);
    let layers = slot(&opened, PaneKind::Layers);
    assert_eq!(
        layers.sizing,
        WindowSizing::Resizable {
            min_width_px: width,
            min_height_px: least,
            max_width_px: 2 * width,
            max_height_px: natural,
        }
    );
    assert_eq!(
        (layers.frame.width, layers.frame.height),
        (width, natural),
        "it opens showing every row"
    );
    let mut chosen = [None; PaneKind::ALL.len()];
    chosen[PaneKind::Layers.index()] = Some((width + 40, least + 10));
    let resized = arranged(
        theme,
        900,
        640,
        Needs {
            chosen,
            ..needs(17)
        },
        &panes,
    );
    let frame = slot(&resized, PaneKind::Layers).frame;
    assert_eq!((frame.width, frame.height), (width + 40, least + 10));
    assert_eq!(
        resized.layers().bounds,
        frame.inset(gap / 2),
        "laid out within it"
    );
    assert!(
        !resized.layers().bar.is_empty(),
        "its rows scrolling where they overflow"
    );
}
