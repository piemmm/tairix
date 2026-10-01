use tairix_font::BitmapFont;
use tairix_geometry::Scale;
use tairix_theme::{TextRole, ThemeRegistry};

use super::{Faces, Layout, PanelNeeds};
use crate::tool::{strip, Tool};

fn faces() -> Faces {
    let registry = ThemeRegistry::with_builtins();
    Faces {
        status: BitmapFont::for_role(registry.active().fonts(), TextRole::Caption, Scale::ONE),
    }
}

#[test]
fn the_bands_tile_the_window_without_overlapping() {
    let registry = ThemeRegistry::with_builtins();
    let theme = registry.active();
    let needs = PanelNeeds {
        swatches: 40,
        settings: 90,
    };
    let layout = Layout::for_window(900, 640, theme, Scale::ONE, faces(), needs);
    let window = layout.window();
    for band in [
        layout.toolbar(),
        layout.panel(),
        layout.canvas(),
        layout.vertical_bar(),
        layout.horizontal_bar(),
        layout.status(),
    ] {
        assert!(!band.is_empty());
        assert_eq!(band.intersection(&window), band, "inside the window");
    }
    assert!(layout.canvas().intersection(&layout.panel()).is_empty());
    assert!(layout.canvas().intersection(&layout.toolbar()).is_empty());
    assert!(layout.canvas().intersection(&layout.status()).is_empty());
    assert_eq!(layout.swatches().height, 40);
    assert_eq!(layout.settings().height, 90);
    assert!(layout.swatches().top() > layout.wells().bottom());
    assert_eq!(
        layout.primary_well().intersection(&layout.wells()),
        layout.primary_well()
    );
    assert_eq!(layout.canvas().right(), layout.vertical_bar().left());
}

#[test]
fn a_tiny_window_gives_up_its_canvas_first() {
    let registry = ThemeRegistry::with_builtins();
    let theme = registry.active();
    let (width, height) = Layout::min_size(theme, Scale::ONE, faces(), &strip(Tool::Pencil));
    let layout = Layout::for_window(
        width,
        height,
        theme,
        Scale::ONE,
        faces(),
        PanelNeeds::default(),
    );
    assert!(
        !layout.canvas().is_empty(),
        "the smallest window still shows a canvas"
    );
    let squeezed = Layout::for_window(40, 30, theme, Scale::ONE, faces(), PanelNeeds::default());
    assert!(squeezed.canvas().width < 40);
}

#[test]
fn a_panel_asking_for_more_than_there_is_is_cut_to_the_panel() {
    let registry = ThemeRegistry::with_builtins();
    let theme = registry.active();
    let needs = PanelNeeds {
        swatches: 10_000,
        settings: 10_000,
    };
    let layout = Layout::for_window(900, 640, theme, Scale::ONE, faces(), needs);
    assert!(layout.swatches().bottom() <= layout.panel().bottom());
    assert!(layout.settings().is_empty() || layout.settings().bottom() <= layout.panel().bottom());
}
