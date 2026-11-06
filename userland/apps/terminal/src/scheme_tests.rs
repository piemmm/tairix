//! Unit tests for the terminal's colour schemes.

use tairix_colour::legibility::contrast_hundredths;
use tairix_colour::Rgb;
use tairix_raster::Color;
use tairix_theme::Palette;
use tairix_vt::{Attributes, BasicColor, Color as VtColor};

use super::{ColorScheme, Painted, Scheme, ANSI_COLORS, XTERM_ANSI};

// --- Scheme::from_name / name / label -------------------------------------

#[test]
fn from_name_and_name_round_trip_for_every_scheme() {
    for scheme in Scheme::ALL {
        assert_eq!(Scheme::from_name(scheme.name()), Some(scheme));
    }
}

#[test]
fn from_name_rejects_an_unknown_spelling() {
    assert_eq!(Scheme::from_name("not-a-scheme"), None);
    assert_eq!(Scheme::from_name(""), None);
    // Case matters: the registry is case-sensitive.
    assert_eq!(Scheme::from_name("Midnight"), None);
}

#[test]
fn every_scheme_label_is_non_empty_and_distinct() {
    let labels: alloc::vec::Vec<&str> = Scheme::ALL.iter().map(|s| s.label()).collect();
    for label in &labels {
        assert!(!label.is_empty());
    }
    for (index, label) in labels.iter().enumerate() {
        for other in labels.iter().skip(index + 1) {
            assert_ne!(label, other, "labels must be distinct");
        }
    }
}

// --- Scheme::palette -------------------------------------------------------

#[test]
fn only_the_users_own_scheme_has_no_fixed_palette() {
    for scheme in Scheme::ALL {
        assert_eq!(
            scheme.palette().is_none(),
            scheme == Scheme::Custom,
            "{scheme:?}"
        );
    }
}

/// The defect: the list of fixed palettes left Paper out, so a test walking
/// it never looked at Paper's colours.
#[test]
fn the_builtins_are_exactly_the_schemes_with_a_fixed_palette() {
    for scheme in Scheme::ALL {
        assert_eq!(
            Scheme::BUILTINS.contains(&scheme),
            scheme.palette().is_some(),
            "{scheme:?}"
        );
    }
}

/// System is the dark theme's own page, text, and accent over the xterm
/// colours, whatever appearance the desktop is in: no theme is consulted.
#[test]
fn the_system_scheme_is_the_dark_themes_page_text_and_accent() {
    let system = Scheme::System.palette().expect("a fixed palette");
    let dark = Palette::DARK;
    // The grid is a page, so its ground is the document role and not the
    // window surface around it.
    assert_eq!(system.background, dark.document.without_alpha());
    assert_ne!(system.background, dark.surface.without_alpha());
    assert_eq!(system.foreground, dark.on_surface.without_alpha());
    assert_eq!(system.cursor, dark.accent.without_alpha());
    assert_eq!(system.cursor_text, dark.on_accent.without_alpha());
    assert_eq!(system.ansi, XTERM_ANSI);
}

// --- Painted::resolve -------------------------------------------------------

/// A custom scheme sharing no colour with any built-in.
fn custom() -> ColorScheme {
    ColorScheme {
        background: Rgb::new(1, 2, 3),
        foreground: Rgb::new(4, 5, 6),
        cursor: Rgb::new(7, 8, 9),
        cursor_text: Rgb::new(10, 11, 12),
        ansi: [Rgb::new(1, 1, 1); ANSI_COLORS],
    }
}

#[test]
fn resolve_custom_takes_the_passed_in_custom_scheme() {
    let painted = Painted::resolve(Scheme::Custom, &custom(), 255);
    assert_eq!(painted.scheme, custom());
}

#[test]
fn resolve_builtin_takes_its_own_fixed_palette() {
    for scheme in Scheme::BUILTINS {
        let painted = Painted::resolve(scheme, &custom(), 255);
        assert_eq!(
            painted.scheme,
            scheme.palette().expect("builtin has a palette")
        );
    }
}

// --- Painted::cell_colors ---------------------------------------------------

/// A [`Painted`] over a fixed built-in scheme, fully opaque unless a test
/// asks for translucency.
fn painted(background_alpha: u8) -> Painted {
    Painted {
        scheme: Scheme::Contrast.palette().expect("contrast has a palette"),
        background_alpha,
    }
}

#[test]
fn default_attributes_take_the_scheme_foreground_and_background() {
    let p = painted(200);
    let (fg, bg) = p.cell_colors(Attributes::PLAIN);
    assert_eq!(fg, Color::from(p.scheme.foreground));
    // The default background carries the translucent alpha in force, while
    // the foreground text stays fully opaque.
    assert_eq!(bg, Color::from(p.scheme.background.with_alpha(200)));
    assert_eq!(fg.a, 255);
    assert_eq!(bg.a, 200);
}

#[test]
fn an_explicit_basic_color_maps_to_its_ansi_slot() {
    let p = painted(255);
    let attrs = Attributes {
        foreground: VtColor::Basic(BasicColor::Green),
        ..Attributes::PLAIN
    };
    let (fg, _bg) = p.cell_colors(attrs);
    assert_eq!(fg, Color::from(p.scheme.ansi(BasicColor::Green.index())));
}

#[test]
fn bold_brightens_a_non_bright_basic_color() {
    let p = painted(255);
    let attrs = Attributes {
        foreground: VtColor::Basic(BasicColor::Red),
        bold: true,
        ..Attributes::PLAIN
    };
    let (fg, _bg) = p.cell_colors(attrs);
    assert_eq!(
        fg,
        Color::from(p.scheme.ansi(BasicColor::BrightRed.index()))
    );
}

#[test]
fn bold_leaves_an_already_bright_basic_color_unchanged() {
    let p = painted(255);
    let attrs = Attributes {
        foreground: VtColor::Basic(BasicColor::BrightGreen),
        bold: true,
        ..Attributes::PLAIN
    };
    let (fg, _bg) = p.cell_colors(attrs);
    assert_eq!(
        fg,
        Color::from(p.scheme.ansi(BasicColor::BrightGreen.index()))
    );
}

#[test]
fn indexed_below_sixteen_uses_the_schemes_own_slot() {
    let p = painted(255);
    for index in 0u8..16 {
        let attrs = Attributes {
            foreground: VtColor::Indexed(index),
            ..Attributes::PLAIN
        };
        let (fg, _bg) = p.cell_colors(attrs);
        assert_eq!(fg, Color::from(p.scheme.ansi(index)));
    }
}

#[test]
fn indexed_in_the_cube_range_uses_the_fixed_6x6x6_arithmetic() {
    let p = painted(255);
    // Index 67 = 16 + 51; offset 51 decomposes to cube coordinates (1, 2, 3).
    let attrs = Attributes {
        foreground: VtColor::Indexed(67),
        ..Attributes::PLAIN
    };
    let (fg, _bg) = p.cell_colors(attrs);
    assert_eq!(fg, tairix_raster::Color::rgb(95, 135, 175));

    // Index 16 is the cube's own black corner.
    let attrs = Attributes {
        foreground: VtColor::Indexed(16),
        ..Attributes::PLAIN
    };
    let (fg, _bg) = p.cell_colors(attrs);
    assert_eq!(fg, tairix_raster::Color::rgb(0, 0, 0));
}

#[test]
fn indexed_in_the_greyscale_range_uses_the_fixed_ramp() {
    let p = painted(255);
    let attrs = Attributes {
        foreground: VtColor::Indexed(232),
        ..Attributes::PLAIN
    };
    let (fg, _bg) = p.cell_colors(attrs);
    assert_eq!(fg, tairix_raster::Color::rgb(8, 8, 8));

    let attrs = Attributes {
        foreground: VtColor::Indexed(255),
        ..Attributes::PLAIN
    };
    let (fg, _bg) = p.cell_colors(attrs);
    assert_eq!(fg, tairix_raster::Color::rgb(238, 238, 238));
}

#[test]
fn truecolor_is_used_verbatim() {
    let p = painted(255);
    let attrs = Attributes {
        foreground: VtColor::Rgb(10, 20, 30),
        background: VtColor::Rgb(40, 50, 60),
        ..Attributes::PLAIN
    };
    let (fg, bg) = p.cell_colors(attrs);
    assert_eq!(fg, tairix_raster::Color::rgb(10, 20, 30));
    // An explicit background colour is opaque, unlike the translucent default.
    assert_eq!(bg, tairix_raster::Color::rgb(40, 50, 60));
}

#[test]
fn reverse_swaps_the_pair_and_forces_both_opaque() {
    let p = painted(120);
    let attrs = Attributes {
        reverse: true,
        ..Attributes::PLAIN
    };
    let (fg, bg) = p.cell_colors(attrs);
    // Reversed: the (translucent) background colour comes out as the first
    // element and the (opaque) foreground colour as the second, and both are
    // forced fully opaque regardless of the window's own translucency.
    assert_eq!(fg.a, 255);
    assert_eq!(bg.a, 255);
    assert_eq!(
        (fg.r, fg.g, fg.b),
        (
            p.scheme.background.r,
            p.scheme.background.g,
            p.scheme.background.b
        )
    );
    assert_eq!(
        (bg.r, bg.g, bg.b),
        (
            p.scheme.foreground.r,
            p.scheme.foreground.g,
            p.scheme.foreground.b
        )
    );
}

// --- Legibility ------------------------------------------------------------

/// Every fixed palette draws its text at WCAG 2.1's 4.5:1 or better against
/// its own ground.
#[test]
fn every_fixed_palette_draws_legible_text_on_its_own_ground() {
    for scheme in Scheme::BUILTINS {
        let palette = scheme.palette().expect("builtin has a palette");
        let contrast = contrast_hundredths(palette.foreground, palette.background);
        assert!(
            contrast >= 450,
            "{scheme:?} text is {contrast} hundredths against its ground"
        );
    }
}
