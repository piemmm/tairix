//! Host tests for painting: that what a paint draws says what the window
//! holds. The solid test face draws each glyph as a block of its ink, so a
//! cell's middle pixel is the colour its character was drawn in.

use alloc::string::String;

use tairix_abi::time::Duration64;
use tairix_font::BitmapFont;
use tairix_geometry::{Region, Scale};
use tairix_input::{InputEvent, PointerButton};
use tairix_raster::{Color, Pixel, Surface};
use tairix_syntax::Format;
use tairix_theme::{SyntaxRole, TextRole, Theme, ThemeRegistry};

use super::render_into;
use crate::document::Document;
use crate::editor::{Editor, Mode};
use crate::layout::{Faces, Layout};
use crate::view::{Access, View};

const WINDOW: (u32, u32) = (800, 480);

fn faces(theme: &Theme) -> Faces {
    Faces {
        grid: BitmapFont::for_role(theme.fonts(), TextRole::Monospace, Scale::ONE),
        status: BitmapFont::for_role(theme.fonts(), TextRole::Caption, Scale::ONE),
    }
}

fn window(text: &[u8]) -> (View, Layout, ThemeRegistry) {
    window_sized(text, WINDOW)
}

fn window_sized(text: &[u8], size: (u32, u32)) -> (View, Layout, ThemeRegistry) {
    let registry = ThemeRegistry::with_builtins();
    let document = Document::from_chunks(alloc::vec![text.to_vec()]).expect("loads");
    let mut view = View::new(
        Editor::new(document, Format::PlainText),
        String::from("doc"),
        Access::Writable,
        Duration64::from_millis(500),
    );
    let layout = view.layout(
        size.0,
        size.1,
        registry.active(),
        Scale::ONE,
        faces(registry.active()),
    );
    view.settle(&layout, &mut Region::new());
    (view, layout, registry)
}

fn paint(view: &View, layout: &Layout, theme: &Theme, focused: bool) -> Surface {
    let size = (
        u32::try_from(layout.window().right()).expect("on screen"),
        u32::try_from(layout.window().bottom()).expect("on screen"),
    );
    let mut surface = Surface::new(size.0, size.1).expect("a surface");
    render_into(
        &mut surface,
        view,
        layout,
        theme,
        Scale::ONE,
        faces(theme),
        focused,
    );
    surface
}

/// The pixel in the middle of grid cell `column` of row `row`.
fn cell(surface: &Surface, layout: &Layout, row: usize, column: usize) -> Pixel {
    let (cell_w, cell_h) = layout.cell();
    let grid = layout.grid();
    let x = u32::try_from(grid.left()).expect("on screen")
        + u32::try_from(column).expect("small") * cell_w
        + cell_w / 2;
    let y = u32::try_from(grid.top()).expect("on screen")
        + u32::try_from(row).expect("small") * cell_h
        + cell_h / 2;
    surface.pixels()[(y * surface.width() + x) as usize]
}

fn ink(theme: &Theme, role: SyntaxRole) -> Pixel {
    Color::from(theme.palette().syntax(role)).premultiply()
}

#[test]
fn a_control_byte_is_drawn_as_its_token_in_the_control_colour() {
    // Two odd bytes in four is binary by the head check; the text view is
    // chosen so what it draws for them can be seen.
    let (mut view, layout, registry) = window(b"a\x03b\xff");
    let theme = registry.active();
    view.act(
        crate::view::Action::Mode(Mode::Text),
        &layout,
        &mut Region::new(),
    );
    let layout = view.layout(WINDOW.0, WINDOW.1, theme, Scale::ONE, faces(theme));
    let surface = paint(&view, &layout, theme, false);
    assert_eq!(cell(&surface, &layout, 0, 0), ink(theme, SyntaxRole::Plain));
    for column in 1..6 {
        assert_eq!(
            cell(&surface, &layout, 0, column),
            ink(theme, SyntaxRole::Control),
            "[x03] cell {column}"
        );
    }
    assert_eq!(cell(&surface, &layout, 0, 6), ink(theme, SyntaxRole::Plain));
    assert_eq!(
        cell(&surface, &layout, 0, 7),
        ink(theme, SyntaxRole::Invalid),
        "[xFF]"
    );
}

#[test]
fn a_selected_blank_differs_from_an_unselected_one() {
    let (mut view, layout, registry) = window(b"a b c");
    let theme = registry.active();
    view.editor_mut().click(0, false);
    view.editor_mut().click(2, true);
    let surface = paint(&view, &layout, theme, false);
    assert_ne!(
        cell(&surface, &layout, 0, 1),
        cell(&surface, &layout, 0, 3),
        "the selected space is lit"
    );
}

#[test]
fn the_caret_shows_only_while_the_window_has_the_keyboard() {
    let (view, layout, registry) = window(b"");
    let theme = registry.active();
    let (cell_w, cell_h) = layout.cell();
    let grid = layout.grid();
    let x = u32::try_from(grid.left()).expect("on screen");
    let y = u32::try_from(grid.top()).expect("on screen") + cell_h / 2;
    let at = |surface: &Surface| surface.pixels()[(y * surface.width() + x) as usize];
    let focused = paint(&view, &layout, theme, true);
    let unfocused = paint(&view, &layout, theme, false);
    assert_eq!(
        at(&focused),
        Color::from(theme.palette().accent).premultiply()
    );
    assert_ne!(at(&unfocused), at(&focused));
    assert!(cell_w > 0);
}

#[test]
fn the_hex_view_draws_digits_and_ascii_by_byte_class() {
    let (mut view, layout, registry) = window(b"A\x00");
    let theme = registry.active();
    let mut damage = Region::new();
    view.act(crate::view::Action::Mode(Mode::Hex), &layout, &mut damage);
    let layout = view.layout(WINDOW.0, WINDOW.1, theme, Scale::ONE, faces(theme));
    let surface = paint(&view, &layout, theme, false);
    let hex = crate::hex::HexLayout::for_len(2);
    let plain = Color::from(theme.palette().on_surface).premultiply();
    let muted = Color::from(theme.palette().on_surface_muted).premultiply();
    assert_eq!(
        cell(&surface, &layout, 0, hex.hex_column(0)),
        plain,
        "the 4 of 41"
    );
    assert_eq!(
        cell(&surface, &layout, 0, hex.hex_column(1)),
        muted,
        "a NUL is quiet"
    );
    assert_eq!(
        cell(&surface, &layout, 0, hex.ascii_column(0)),
        plain,
        "its letter"
    );
    assert_eq!(cell(&surface, &layout, 0, 0), muted, "the offset column");
}

#[test]
fn a_modal_question_veils_the_window() {
    let (mut view, layout, registry) = window(b"x");
    let theme = registry.active();
    let clear = paint(&view, &layout, theme, false);
    let mut damage = Region::new();
    view.act(crate::view::Action::GoToLine, &layout, &mut damage);
    let veiled = paint(&view, &layout, theme, false);
    let corner = |surface: &Surface| surface.pixels()[2];
    assert_ne!(corner(&clear), corner(&veiled));
}

/// A paint under a clip draws exactly what a whole paint draws inside it, and
/// nothing outside: what lets a keystroke repaint its own row alone.
#[test]
fn a_clipped_paint_draws_its_rows_as_a_whole_paint_would_and_nothing_else() {
    let (view, layout, registry) = window(b"first line\nsecond line\nthird line\n");
    let theme = registry.active();
    let whole = paint(&view, &layout, theme, true);
    let row = layout.row_rect(1);
    let sentinel = Color::rgb(0x12, 0x34, 0x56);
    let mut clipped = Surface::new(WINDOW.0, WINDOW.1).expect("a surface");
    clipped.fill(sentinel);
    let (x, y) = (
        u32::try_from(row.left()).expect("on screen"),
        u32::try_from(row.top()).expect("on screen"),
    );
    clipped.with_clip(x, y, row.width, row.height, |surface| {
        render_into(
            surface,
            &view,
            &layout,
            theme,
            Scale::ONE,
            faces(theme),
            true,
        );
    });
    for py in 0..WINDOW.1 {
        for px in 0..WINDOW.0 {
            let inside = row.contains(tairix_geometry::Point::new(
                i32::try_from(px).expect("fits"),
                i32::try_from(py).expect("fits"),
            ));
            let expected = if inside {
                whole.get(px, py)
            } else {
                Some(sentinel.premultiply())
            };
            assert_eq!(clipped.get(px, py), expected, "({px}, {py})");
        }
    }
}

/// Scroll `columns` columns right, a press of the horizontal bar's end
/// button each.
fn scroll_right(view: &mut View, layout: &Layout, theme: &Theme, columns: usize) {
    let bar = layout.horizontal_bar();
    let button = tairix_geometry::Point::new(bar.right() - 2, bar.center().y);
    let mut damage = Region::new();
    for event in [InputEvent::PointerMoved { to: button }]
        .into_iter()
        .chain((0..columns).flat_map(|_| {
            [
                InputEvent::PointerPressed {
                    button: PointerButton::Primary,
                },
                InputEvent::PointerReleased {
                    button: PointerButton::Primary,
                },
            ]
        }))
    {
        view.on_pointer(&event, 0, layout, Scale::ONE, theme, &mut damage);
    }
    assert_eq!(view.scroll().2, columns, "scrolled a column a press");
}

#[test]
fn a_wide_character_straddling_the_left_edge_leaves_its_run_in_place() {
    // Tabs draw nothing and make the line wider than the grid.
    let mut text = "\u{4e2d}bc".as_bytes().to_vec();
    text.extend(core::iter::repeat_n(b'\t', 30));
    let (mut view, layout, registry) = window(&text);
    let theme = registry.active();
    scroll_right(&mut view, &layout, theme, 1);
    let surface = paint(&view, &layout, theme, false);
    let plain = ink(theme, SyntaxRole::Plain);
    assert_eq!(
        cell(&surface, &layout, 0, 0),
        plain,
        "the character's right half"
    );
    assert_eq!(cell(&surface, &layout, 0, 1), plain, "b");
    assert_eq!(cell(&surface, &layout, 0, 2), plain, "c");
    assert_ne!(cell(&surface, &layout, 0, 3), plain, "nothing after c");
}

#[test]
fn a_hex_selection_scrolled_off_the_left_is_not_drawn_at_the_edge() {
    let (mut view, _, registry) = window_sized(&[b'A'; 32], (360, 240));
    let theme = registry.active();
    let mut damage = Region::new();
    let layout = view.layout(360, 240, theme, Scale::ONE, faces(theme));
    view.act(crate::view::Action::Mode(Mode::Hex), &layout, &mut damage);
    let layout = view.layout(360, 240, theme, Scale::ONE, faces(theme));
    view.settle(&layout, &mut damage);
    let hex = crate::hex::HexLayout::for_len(32);
    // The gap before byte 1's digits becomes the grid's first column.
    scroll_right(&mut view, &layout, theme, hex.hex_column(1) - 1);
    let plain = paint(&view, &layout, theme, false);
    view.editor_mut().click(0, false);
    view.editor_mut().click(1, true);
    assert_eq!(view.editor().selection().range(), 0..1);
    let selected = paint(&view, &layout, theme, false);
    assert_eq!(
        cell(&selected, &layout, 0, 0),
        cell(&plain, &layout, 0, 0),
        "byte 0's shading is off the grid"
    );
}
