use alloc::string::String;

use tairix_controls::Checker;
use tairix_font::BitmapFont;
use tairix_geometry::{Point, Region, Scale};
use tairix_icon::NoArtwork;
use tairix_input::{InputEvent, PointerButton};
use tairix_raster::{Color, Pixel, Surface};
use tairix_theme::{TextRole, Theme, ThemeRegistry};
use tairix_window::document::Access;

use super::render_into;
use crate::canvas::{Canvas, CanvasBuilder, Kind, Sample};
use crate::document::{Document, Picture};
use crate::layout::{Faces, Layout};
use crate::tool::Tool;
use crate::view::{Action, View};

const WINDOW: (u32, u32) = (900, 640);

fn faces(theme: &Theme) -> Faces {
    Faces {
        status: BitmapFont::for_role(theme.fonts(), TextRole::Caption, Scale::ONE),
    }
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
