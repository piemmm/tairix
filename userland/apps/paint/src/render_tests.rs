use alloc::string::String;

use tairix_controls::Checker;
use tairix_geometry::{Point, Region, Scale};
use tairix_icon::NoArtwork;
use tairix_input::{InputEvent, PointerButton};
use tairix_raster::{Color, Pixel, Surface};
use tairix_theme::{Theme, ThemeRegistry};
use tairix_window::document::Access;

use super::render_into;
use crate::canvas::{Canvas, CanvasBuilder, Kind, Sample};
use crate::document::{Document, Picture};
use crate::layout::{Faces, Layout};
use crate::tool::Tool;
use crate::view::{Action, View};

const WINDOW: (u32, u32) = (900, 640);

fn faces(theme: &Theme) -> Faces {
    Faces::of(theme, Scale::ONE)
}

fn window(canvas: Canvas) -> (View, Layout, ThemeRegistry) {
    let registry = ThemeRegistry::with_builtins();
    let mut view = View::new(
        Document::new(Picture::plain(canvas)),
        String::from("p.png"),
        Access::Writable,
    );
    let theme = registry.active();
    let layout = view.layout(WINDOW.0, WINDOW.1, theme, Scale::ONE, faces(theme));
    view.settle(&layout, &mut Region::new());
    (view, layout, registry)
}

fn paint(view: &View, layout: &Layout, theme: &Theme) -> Surface {
    let mut surface = Surface::new(WINDOW.0, WINDOW.1).expect("a surface");
    render_into(
        &mut surface,
        view,
        layout,
        theme,
        Scale::ONE,
        faces(theme),
        &mut NoArtwork,
    );
    surface
}

/// The screen pixel at the top left of picture pixel `(x, y)`.
fn corner_of(view: &View, layout: &Layout, size: (u32, u32), (x, y): (u32, u32)) -> (u32, u32) {
    let (ox, oy) = view.viewport().origin(size, layout.canvas());
    let (across, down) = view.viewport().pixel_span();
    (
        u32::try_from(ox + i64::from(x) * i64::try_from(across).expect("small"))
            .expect("on screen"),
        u32::try_from(oy + i64::from(y) * i64::try_from(down).expect("small")).expect("on screen"),
    )
}

#[test]
fn each_picture_pixel_is_drawn_where_the_viewport_puts_it() {
    let mut built = CanvasBuilder::new(20, 10, Kind::Rgba, Sample::Rgba([255; 4])).expect("fits");
    built.set(3, 4, Sample::Rgba([200, 10, 10, 255]));
    let (view, layout, registry) = window(built.finish());
    let theme = registry.active();
    let surface = paint(&view, &layout, theme);
    let (x, y) = corner_of(&view, &layout, (20, 10), (3, 4));
    assert_eq!(
        surface.get(x, y),
        Some(Color::rgb(200, 10, 10).premultiply())
    );
    let (x, y) = corner_of(&view, &layout, (20, 10), (4, 4));
    assert_eq!(
        surface.get(x, y),
        Some(Color::rgb(255, 255, 255).premultiply())
    );
}

#[test]
fn the_checkerboard_shows_through_a_clear_pixel() {
    let canvas = Canvas::new(40, 40, Kind::Rgba, Sample::Rgba([0; 4])).expect("fits");
    let (view, layout, registry) = window(canvas);
    let theme = registry.active();
    let surface = paint(&view, &layout, theme);
    let board = Checker::new(theme, Scale::ONE);
    let (x, y) = corner_of(&view, &layout, (40, 40), (0, 0));
    assert_eq!(surface.get(x, y), Some(board.at(0, 0).premultiply()));
    let side = board.side();
    assert_eq!(
        surface.get(x + side, y),
        Some(board.at(side, 0).premultiply())
    );
}

fn magnified(canvas: Canvas, rung: usize) -> (View, Layout, ThemeRegistry) {
    let (mut view, layout, registry) = window(canvas);
    view.act(Action::Zoom(rung), &layout, &mut Region::new());
    view.settle(&layout, &mut Region::new());
    (view, layout, registry)
}

#[test]
fn a_magnified_pixel_fills_its_block_and_the_grid_runs_between_blocks() {
    let mut built = CanvasBuilder::new(8, 8, Kind::Rgba, Sample::Rgba([255; 4])).expect("fits");
    built.set(1, 1, Sample::Rgba([0, 0, 255, 255]));
    let (mut view, layout, registry) = magnified(built.finish(), 9);
    let theme = registry.active();
    assert_eq!(view.viewport().pixel_span(), (8, 8));
    let surface = paint(&view, &layout, theme);
    let (x, y) = corner_of(&view, &layout, (8, 8), (1, 1));
    let blue = Color::rgb(0, 0, 255).premultiply();
    for (dx, dy) in [(0, 0), (7, 0), (0, 7), (7, 7), (3, 4)] {
        assert_eq!(surface.get(x + dx, y + dy), Some(blue), "({dx}, {dy})");
    }
    view.act(Action::Grid, &layout, &mut Region::new());
    let gridded = paint(&view, &layout, theme);
    assert_ne!(
        gridded.get(x, y + 3),
        Some(blue),
        "a line down the block's left edge"
    );
    assert_eq!(gridded.get(x + 3, y + 3), Some(blue), "the block's inside");
}

#[test]
fn a_reduced_view_averages_the_pixels_each_screen_pixel_covers() {
    let mut built =
        CanvasBuilder::new(64, 64, Kind::Rgba, Sample::Rgba([0, 0, 0, 255])).expect("fits");
    for y in 0..64 {
        for x in (0..64).step_by(2) {
            built.set(x, y, Sample::Rgba([255, 255, 255, 255]));
        }
    }
    let (view, layout, registry) = magnified(built.finish(), 3);
    let theme = registry.active();
    let surface = paint(&view, &layout, theme);
    let (x, y) = corner_of(&view, &layout, (64, 64), (0, 0));
    let Some(Pixel { r, a, .. }) = surface.get(x + 2, y + 2) else {
        panic!("on screen");
    };
    assert_eq!(a, 255);
    assert!(
        (100..=156).contains(&r),
        "black and white stripes average to grey: {r}"
    );
}

/// A reduced view's last screen column takes every tap its neighbours do,
/// so the picture's right edge is no darker than the rest of it.
#[test]
fn a_reduced_views_right_edge_averages_as_its_neighbours_do() {
    let mut built =
        CanvasBuilder::new(64, 64, Kind::Rgba, Sample::Rgba([0, 0, 0, 255])).expect("fits");
    for y in 0..64 {
        for x in (0..64).step_by(2) {
            built.set(x, y, Sample::Rgba([255, 255, 255, 255]));
        }
    }
    let picture = built.finish();
    // A half reads every column it covers; an eighth gathers only its taps.
    for rung in [3, 1] {
        let (view, layout, registry) = magnified(picture.try_clone().expect("room"), rung);
        let theme = registry.active();
        let surface = paint(&view, &layout, theme);
        let placed = view.viewport().to_screen(
            crate::shape::Bounds::picture(64, 64),
            (64, 64),
            layout.canvas(),
        );
        let (left, top) = placed.surface_origin().expect("on screen");
        let right = left + placed.width - 1;
        let row = top + placed.height / 2;
        let (Some(edge), Some(inside)) = (surface.get(right, row), surface.get(right - 1, row))
        else {
            panic!("on screen");
        };
        assert_eq!(
            edge, inside,
            "rung {rung}: the edge averages as its neighbour"
        );
    }
}

/// Where the picture's edge cuts a reduced screen pixel's footprint short,
/// that pixel samples what of the footprint the picture holds rather than
/// its last column alone.
#[test]
fn a_footprint_the_edge_cuts_short_samples_what_it_holds() {
    let mut built =
        CanvasBuilder::new(100, 16, Kind::Rgba, Sample::Rgba([0, 0, 0, 255])).expect("fits");
    for y in 0..16 {
        built.set(99, y, Sample::Rgba([255, 255, 255, 255]));
    }
    let picture = built.finish();
    // A sixteenth: the last screen column covers columns 96 to 99 alone.
    let (view, layout, registry) = magnified(picture, 0);
    let surface = paint(&view, &layout, registry.active());
    let placed = view.viewport().to_screen(
        crate::shape::Bounds::picture(100, 16),
        (100, 16),
        layout.canvas(),
    );
    let (left, top) = placed.surface_origin().expect("on screen");
    let edge = surface
        .get(left + placed.width - 1, top)
        .expect("on screen");
    assert!(
        edge.r > 0 && edge.r < u8::MAX,
        "the black and the white the edge's footprint holds, mixed: {}",
        edge.r
    );
}

#[test]
fn a_shape_being_dragged_is_drawn_as_it_will_be_put_down() {
    let canvas = Canvas::new(60, 60, Kind::Rgba, Sample::Rgba([255; 4])).expect("fits");
    let (mut view, layout, registry) = window(canvas);
    let theme = registry.active();
    view.act(Action::Tool(Tool::Ellipse), &layout, &mut Region::new());
    let layout = view.layout(WINDOW.0, WINDOW.1, theme, Scale::ONE, faces(theme));
    view.settle(&layout, &mut Region::new());
    let point = |(x, y): (u32, u32)| {
        let (sx, sy) = corner_of(&view, &layout, (60, 60), (x, y));
        Point::new(
            i32::try_from(sx).expect("fits"),
            i32::try_from(sy).expect("fits"),
        )
    };
    let (from, to) = (point((5, 5)), point((40, 30)));
    let feed = |view: &mut View, event: InputEvent| {
        view.on_pointer(&event, &layout, Scale::ONE, theme, &mut Region::new());
    };
    feed(&mut view, InputEvent::PointerMoved { to: from });
    feed(
        &mut view,
        InputEvent::PointerPressed {
            button: PointerButton::Primary,
        },
    );
    feed(&mut view, InputEvent::PointerMoved { to });
    let previewed = paint(&view, &layout, theme);
    feed(
        &mut view,
        InputEvent::PointerReleased {
            button: PointerButton::Primary,
        },
    );
    let drawn = paint(&view, &layout, theme);
    let canvas = layout.canvas();
    for y in canvas.top()..canvas.bottom() {
        for x in canvas.left()..canvas.right() {
            let (x, y) = (u32::try_from(x).expect("on"), u32::try_from(y).expect("on"));
            assert_eq!(previewed.get(x, y), drawn.get(x, y), "({x}, {y})");
        }
    }
}

#[test]
fn a_clipped_paint_draws_what_a_whole_one_does_inside_the_clip() {
    let canvas = Canvas::new(300, 200, Kind::Rgba, Sample::Rgba([10, 200, 30, 128])).expect("fits");
    let (view, layout, registry) = window(canvas);
    let theme = registry.active();
    let whole = paint(&view, &layout, theme);
    let mut clipped = Surface::new(WINDOW.0, WINDOW.1).expect("a surface");
    let area = layout.canvas();
    let (x, y) = (
        u32::try_from(area.left()).expect("on") + 40,
        u32::try_from(area.top()).expect("on") + 30,
    );
    clipped.with_clip(x, y, 70, 50, |surface| {
        render_into(
            surface,
            &view,
            &layout,
            theme,
            Scale::ONE,
            faces(theme),
            &mut NoArtwork,
        );
    });
    for py in y..y + 50 {
        for px in x..x + 70 {
            assert_eq!(clipped.get(px, py), whole.get(px, py), "({px}, {py})");
        }
    }
    assert_eq!(
        clipped.get(x - 1, y),
        Some(Color::TRANSPARENT.premultiply())
    );
}

/// Whether any pixel of `rect` on `surface` is `want`.
fn any_in(surface: &Surface, rect: tairix_geometry::Rect, want: Pixel) -> bool {
    let (Ok(left), Ok(top)) = (u32::try_from(rect.left()), u32::try_from(rect.top())) else {
        return false;
    };
    (left..left + rect.width)
        .any(|x| (top..top + rect.height).any(|y| surface.get(x, y) == Some(want)))
}

#[test]
fn the_tool_box_marks_the_tool_in_use_on_its_leading_edge() {
    let canvas = Canvas::new(20, 20, Kind::Rgba, Sample::Rgba([255; 4])).expect("fits");
    let (view, layout, registry) = window(canvas);
    let theme = registry.active();
    let surface = paint(&view, &layout, theme);
    let brush = view
        .controls()
        .tool_box
        .tool_rect(2, layout.tools(), Scale::ONE, theme)
        .expect("the brush is seated");
    let accent = Color::from(theme.palette().accent).premultiply();
    let edge = tairix_geometry::Rect::new(brush.left(), brush.top() + 2, 1, brush.height - 4);
    assert!(
        any_in(&surface, edge, accent),
        "the brush, in use, is marked"
    );
    let pencil = view
        .controls()
        .tool_box
        .tool_rect(1, layout.tools(), Scale::ONE, theme)
        .expect("the pencil is seated");
    let edge = tairix_geometry::Rect::new(pencil.left(), pencil.top() + 2, 1, pencil.height - 4);
    assert!(!any_in(&surface, edge, accent), "the pencil is not");
}

#[test]
fn the_bar_names_its_tool_and_draws_its_settings() {
    let canvas = Canvas::new(20, 20, Kind::Rgba, Sample::Rgba([255; 4])).expect("fits");
    let (view, layout, registry) = window(canvas);
    let theme = registry.active();
    let surface = paint(&view, &layout, theme);
    let raised = Color::from(theme.palette().surface_raised).premultiply();
    let name = layout.bar();
    let size = name.control(0).expect("the size is seated");
    let ink = |rect: tairix_geometry::Rect| {
        let (Ok(left), Ok(top)) = (u32::try_from(rect.left()), u32::try_from(rect.top())) else {
            return false;
        };
        (left..left + rect.width).any(|x| {
            (top..top + rect.height).any(|y| surface.get(x, y).is_some_and(|pixel| pixel != raised))
        })
    };
    assert!(ink(size), "the size field is drawn");
    assert!(ink(name.control(1).expect("the switch is seated")));
    let band = layout.controls();
    let caption = tairix_geometry::Rect::new(
        band.left(),
        band.top(),
        size.left().saturating_sub(band.left()).unsigned_abs(),
        band.height,
    );
    assert!(ink(caption), "the tool is named before its settings");
}

#[test]
fn the_palette_strip_shows_each_well_in_its_colour() {
    let canvas = Canvas::new(20, 20, Kind::Rgba, Sample::Rgba([255; 4])).expect("fits");
    let (view, layout, registry) = window(canvas);
    let theme = registry.active();
    let surface = paint(&view, &layout, theme);
    let swatches = view.controls().swatches;
    for well in [2, 11] {
        let cell = swatches.cell_rect(layout.swatches(), well).expect("a well");
        let colour = swatches.colour(well).expect("a colour").premultiply();
        let centre = cell.center();
        let at = (
            u32::try_from(centre.x - 3).expect("on"),
            u32::try_from(centre.y - 3).expect("on"),
        );
        assert_eq!(surface.get(at.0, at.1), Some(colour), "well {well}");
    }
}

#[test]
fn an_open_list_is_drawn_over_the_canvas() {
    let canvas = Canvas::new(200, 200, Kind::Rgba, Sample::Rgba([0, 0, 0, 255])).expect("fits");
    let (mut view, layout, registry) = window(canvas);
    let theme = registry.active();
    view.act(Action::Tool(Tool::Rectangle), &layout, &mut Region::new());
    let layout = view.layout(WINDOW.0, WINDOW.1, theme, Scale::ONE, faces(theme));
    view.settle(&layout, &mut Region::new());
    let style = layout
        .bar()
        .control(1)
        .expect("the style is seated")
        .center();
    for event in [
        InputEvent::PointerMoved { to: style },
        InputEvent::PointerPressed {
            button: PointerButton::Primary,
        },
        InputEvent::PointerReleased {
            button: PointerButton::Primary,
        },
    ] {
        view.on_pointer(&event, &layout, Scale::ONE, theme, &mut Region::new());
    }
    let popup = view
        .controls()
        .bar
        .popup_rect(layout.bar(), Scale::ONE, theme);
    let over = popup.intersection(&layout.canvas());
    assert!(!over.is_empty(), "the list hangs over the canvas");
    let surface = paint(&view, &layout, theme);
    let black = Color::rgb(0, 0, 0).premultiply();
    let middle = over.center();
    assert_ne!(
        surface.get(
            u32::try_from(middle.x).expect("on"),
            u32::try_from(middle.y).expect("on")
        ),
        Some(black),
        "the list, not the picture, is drawn there"
    );
}

#[test]
fn a_line_reaching_far_off_the_canvas_is_clipped_to_what_shows() {
    let clip = tairix_geometry::Rect::new(10, 20, 100, 50);
    let (from, to) = super::clipped((-1_000_000, 45), (1_000_000, 45), clip).expect("crosses");
    assert_eq!(
        (from, to),
        ((10, 45), (109, 45)),
        "held to the clip's columns"
    );
    assert_eq!(super::clipped((0, 0), (5, 5), clip), None, "wholly outside");
    let inside = ((20, 30), (60, 50));
    assert_eq!(
        super::clipped(inside.0, inside.1, clip),
        Some(inside),
        "already inside"
    );
    let (from, to) = super::clipped((60, -500), (60, 5000), clip).expect("crosses");
    assert_eq!((from, to), ((60, 20), (60, 69)));
    let (from, to) = super::clipped((-90, -80), (210, 220), clip).expect("a diagonal crosses");
    for (x, y) in [from, to] {
        assert!(
            (10..110).contains(&x) && (20..70).contains(&y),
            "({x}, {y})"
        );
        assert_eq!(x - y, -10, "on the line");
    }
}

/// A soft selection's outline is drawn where what it chooses meets what it
/// leaves out, and nowhere inside or outside it.
#[test]
fn a_soft_selection_is_outlined_where_it_meets_what_it_leaves_out() {
    use crate::mask::Mask;
    use crate::shape::{Bounds, Shape, ShapeScratch, Span};
    use crate::viewport::Viewport;
    let size = (60, 40);
    let area = tairix_geometry::Rect::new(0, 0, 200, 120);
    let mut viewport = Viewport::new((1, 1));
    viewport.settle(size, area);
    let oval = Shape::Ellipse {
        span: Span {
            from: (5, 5),
            to: (44, 30),
        },
        outline: None,
    };
    let chosen = Mask::shape(
        &oval,
        true,
        Bounds::picture(60, 40),
        &mut ShapeScratch::default(),
    )
    .expect("room")
    .expect("pixels");
    let mut surface = Surface::new(200, 120).expect("a surface");
    let (dark, light) = (Color::rgba(255, 0, 0, 255), Color::rgba(0, 0, 255, 255));
    let mut ants = super::Ants {
        surface: &mut surface,
        clip: area,
        dark,
        light,
    };
    ants.outline(&chosen, &viewport, size);
    let origin = viewport.origin(size, area);
    let drawn = |surface: &Surface, (x, y): (i64, i64)| {
        let at = (
            u32::try_from(origin.0 + x).expect("on"),
            u32::try_from(origin.1 + y).expect("on"),
        );
        let pixel = surface.get(at.0, at.1);
        pixel == Some(dark.premultiply()) || pixel == Some(light.premultiply())
    };
    assert!(!drawn(&surface, (25, 18)), "inside");
    assert!(!drawn(&surface, (2, 2)), "outside");
    let first = (0..60)
        .find(|&x| chosen.chooses(x, 18))
        .expect("the middle row");
    assert!(drawn(&surface, (first, 18)), "its left edge");
    let last = (0..60)
        .rev()
        .find(|&x| chosen.chooses(x, 18))
        .expect("the middle row");
    assert!(drawn(&surface, (last, 18)), "its right edge");
    let dots = (0..60).filter(|&x| drawn(&surface, (x, 18))).count();
    assert_eq!(dots, 2, "an edge each side of a row across the middle");
}

fn layered_window(
    layers: alloc::vec::Vec<crate::document::Layer>,
    active: usize,
) -> (View, Layout, ThemeRegistry) {
    let registry = ThemeRegistry::with_builtins();
    let picture = Picture::layered(layers, active).expect("alike");
    let mut view = View::new(
        Document::new(picture),
        String::from("p.ora"),
        Access::Writable,
    );
    let theme = registry.active();
    let layout = view.layout(WINDOW.0, WINDOW.1, theme, Scale::ONE, faces(theme));
    view.settle(&layout, &mut Region::new());
    (view, layout, registry)
}

fn flat_layer(size: (u32, u32), colour: [u8; 4], opacity: u8) -> crate::document::Layer {
    let canvas = Canvas::new(size.0, size.1, Kind::Rgba, Sample::Rgba(colour)).expect("fits");
    let mut layer = crate::document::Layer::new(canvas, String::from("layer"));
    layer.opacity = opacity;
    layer
}

fn near(pixel: Option<Pixel>, want: [u8; 3]) -> bool {
    pixel.is_some_and(|Pixel { r, g, b, a }| {
        a == 255 && r.abs_diff(want[0]) <= 1 && g.abs_diff(want[1]) <= 1 && b.abs_diff(want[2]) <= 1
    })
}

#[test]
fn the_layers_are_shown_laid_together() {
    let size = (20, 10);
    let layers = alloc::vec![
        flat_layer(size, [0, 0, 255, 255], 255),
        flat_layer(size, [255, 0, 0, 255], 128),
    ];
    let (mut view, layout, registry) = layered_window(layers, 1);
    let theme = registry.active();
    let (x, y) = corner_of(&view, &layout, size, (3, 4));
    assert!(near(paint(&view, &layout, theme).get(x, y), [128, 0, 127]));
    view.act(Action::ShowLayer, &layout, &mut Region::new());
    let hidden = paint(&view, &layout, theme);
    assert_eq!(
        hidden.get(x, y),
        Some(Color::rgb(0, 0, 255).premultiply()),
        "the top hidden"
    );
}

#[test]
fn a_reduced_view_lays_the_layers_together_where_it_samples() {
    let size = (64, 64);
    let mut stripes = CanvasBuilder::new(64, 64, Kind::Rgba, Sample::Rgba([0; 4])).expect("fits");
    for y in 0..64 {
        for x in (0..64).step_by(2) {
            stripes.set(x, y, Sample::Rgba([255; 4]));
        }
    }
    let mut top = crate::document::Layer::new(stripes.finish(), String::from("stripes"));
    top.opacity = 255;
    let (mut view, layout, registry) =
        layered_window(alloc::vec![flat_layer(size, [0, 0, 0, 255], 255), top], 0);
    view.act(Action::Zoom(3), &layout, &mut Region::new());
    view.settle(&layout, &mut Region::new());
    let theme = registry.active();
    let surface = paint(&view, &layout, theme);
    let (x, y) = corner_of(&view, &layout, size, (0, 0));
    let Some(Pixel { r, a, .. }) = surface.get(x + 2, y + 2) else {
        panic!("on screen");
    };
    assert_eq!(a, 255);
    assert!(
        (100..=156).contains(&r),
        "white stripes over black average to grey: {r}"
    );
}

/// A shape dragged on a layer beneath another shows through it exactly as
/// it will once put down.
#[test]
fn a_shape_dragged_beneath_a_layer_shows_as_it_will_be_put_down() {
    let size = (60, 60);
    let layers = alloc::vec![
        flat_layer(size, [255; 4], 255),
        flat_layer(size, [0, 200, 0, 255], 90),
    ];
    let (mut view, first, registry) = layered_window(layers, 0);
    let theme = registry.active();
    view.act(Action::Tool(Tool::Ellipse), &first, &mut Region::new());
    let layout = view_layout(&mut view, theme);
    let point = |(x, y): (u32, u32)| {
        let (sx, sy) = corner_of(&view, &layout, size, (x, y));
        Point::new(
            i32::try_from(sx).expect("fits"),
            i32::try_from(sy).expect("fits"),
        )
    };
    let (from, to) = (point((5, 5)), point((40, 30)));
    let feed = |view: &mut View, event: InputEvent| {
        view.on_pointer(&event, &layout, Scale::ONE, theme, &mut Region::new());
    };
    feed(&mut view, InputEvent::PointerMoved { to: from });
    feed(
        &mut view,
        InputEvent::PointerPressed {
            button: PointerButton::Primary,
        },
    );
    feed(&mut view, InputEvent::PointerMoved { to });
    let previewed = paint(&view, &layout, theme);
    feed(
        &mut view,
        InputEvent::PointerReleased {
            button: PointerButton::Primary,
        },
    );
    let drawn = paint(&view, &layout, theme);
    let canvas = layout.canvas();
    for y in canvas.top()..canvas.bottom() {
        for x in canvas.left()..canvas.right() {
            let (x, y) = (u32::try_from(x).expect("on"), u32::try_from(y).expect("on"));
            assert_eq!(previewed.get(x, y), drawn.get(x, y), "({x}, {y})");
        }
    }
}

fn view_layout(view: &mut View, theme: &Theme) -> Layout {
    let layout = view.layout(WINDOW.0, WINDOW.1, theme, Scale::ONE, faces(theme));
    view.settle(&layout, &mut Region::new());
    layout
}
