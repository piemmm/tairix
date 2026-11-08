//! The engine a window shows passes what the host tells it to the picture or
//! sprite file it holds.

use alloc::boxed::Box;
use alloc::string::String;

use tairix_geometry::{Region, Scale};
use tairix_theme::{Theme, ThemeRegistry};
use tairix_window::docapp::{DocumentView, Relayout};
use tairix_window::document::Access;

use super::Engine;
use crate::canvas::{Canvas, Kind, Sample};
use crate::document::{Document, Picture};
use crate::layout::Faces;
use crate::view::{Action, View};

const WINDOW: (u32, u32) = (900, 640);

fn laid_out(engine: &mut Engine, theme: &Theme) {
    let faces = Faces::of(theme, Scale::ONE);
    engine.lay_out(
        WINDOW.0,
        WINDOW.1,
        theme,
        Scale::ONE,
        faces,
        &mut Region::new(),
    );
}

/// The one tool window the engine wants, the floating palette's: its id and
/// its size.
fn palette(engine: &Engine) -> (u32, (u32, u32)) {
    let wanted = engine.tool_window(0).expect("the palette's tool window");
    assert!(engine.tool_window(1).is_none(), "the palette alone floats");
    (wanted.id, (wanted.rect.width, wanted.rect.height))
}

/// A picture window's floating palette resized by its edges takes that size:
/// the resize reaches the picture's editor through the engine the host
/// drives.
#[test]
fn a_picture_windows_resized_palette_takes_its_size_through_the_engine() {
    let registry = ThemeRegistry::with_builtins();
    let theme = registry.active();
    let canvas = Canvas::new(16, 16, Kind::Rgba, Sample::Rgba([255; 4])).expect("fits");
    let view = View::new(
        Document::new(Picture::plain(canvas)),
        String::from("picture.png"),
        None,
        Access::Writable,
    );
    let mut engine = Engine::Picture(Box::new(view));
    laid_out(&mut engine, theme);
    let _ = engine
        .picture_mut()
        .expect("a picture")
        .act(Action::FloatPalette, &mut Region::new());
    laid_out(&mut engine, theme);
    let (id, (width, height)) = palette(&engine);
    let resized = (width + 40, height + 24);
    let mut damage = Region::new();
    let outcome = engine.tool_resized(id, resized, Scale::ONE, &mut damage);
    assert_eq!(outcome.relayout, Relayout::Reported);
    assert!(!damage.is_empty(), "the palette repaints");
    laid_out(&mut engine, theme);
    assert_eq!(palette(&engine), (id, resized));
}
