//! Unit tests for a window's look: what a desktop change and a profile change
//! each make of it.

use tairix_abi::desktop::DesktopInfo;
use tairix_parallel::SERIAL;
use tairix_raster::{Color, Pixel};
use tairix_theme::{Palette, Theme};
use tairix_window::Desktop;

use super::Look;
use crate::effects::{Effects, FULL};
use crate::grid::Grid;
use crate::profile::{Invalidation, Profile};
use crate::render::Screen;
use crate::scheme::Scheme;

/// A 1024x768 desktop in `theme`'s appearance at `scale_percent` of the
/// reference density.
fn desktop(theme: &Theme, scale_percent: u16) -> Desktop {
    let screen = DesktopInfo::new(1024, 768, scale_percent, theme.appearance());
    let info = screen.expect("a usable screen");
    Desktop::new(info).expect("a scale the desktop admits")
}

/// The default profile drawn opaque, so a page pixel reads back as its own
/// colour, with `scanlines` of scan lines.
fn opaque_profile(scanlines: u16) -> Profile {
    let defaults = Profile::default();
    Profile {
        effects: Effects {
            opacity: FULL,
            scanlines,
            ..defaults.effects
        },
        ..defaults
    }
}

/// A blank row of four cells with its cursor hidden, and a screen sized to it
/// in `look`'s face.
fn blank_row(look: &Look<()>) -> (Grid, Screen) {
    let mut grid = Grid::new(4, 1, 0).expect("valid size");
    grid.set_cursor_visible(false);
    let font = look.font;
    let (width, height) = (font.cell_width() * 4, font.line_height());
    (grid, Screen::new(width, height).expect("surface"))
}

/// Paint `grid` and run the passes over it, as a present does.
fn draw(look: &mut Look<()>, grid: &Grid, screen: &mut Screen) {
    let damage = screen.paint(grid, &look.painted, look.font);
    let frame = look.pipeline.render(
        screen.surface(),
        damage,
        look.effects,
        look.scale.percent(),
        look.phase,
        &SERIAL,
    );
    assert!(frame.is_some(), "a pass is on");
}

/// The defect: a terminal on the System scheme followed a light desktop onto
/// its white paper. Its page is the dark theme's whatever the desktop shows.
#[test]
fn a_system_cell_on_a_light_desktop_paints_the_dark_page() {
    let profile = opaque_profile(0);
    assert_eq!(profile.scheme, Scheme::System);
    let light = Theme::light();
    let look = Look::resolve(&profile, &light, &desktop(&light, 100), ());
    let (grid, mut screen) = blank_row(&look);
    let _ = screen.paint(&grid, &look.painted, look.font);
    let page = screen.surface().get(0, 0).map(Pixel::unpremultiply);
    assert_eq!(page, Some(Color::from(Palette::DARK.document)));
    assert_ne!(Palette::DARK.document, light.palette().document);
}

/// A light/dark switch moves nothing a terminal draws: nothing is stale, and
/// the face and the colours are the ones it had.
#[test]
fn a_light_dark_switch_invalidates_nothing_and_keeps_the_face() {
    let profile = opaque_profile(0);
    for (was, now) in [
        (Theme::light(), Theme::dark()),
        (Theme::dark(), Theme::light()),
    ] {
        let (from, to) = (was.appearance(), now.appearance());
        let mut look = Look::resolve(&profile, &was, &desktop(&was, 100), ());
        let (font, painted) = (look.font, look.painted);
        let changed = look.refresh(&now, &desktop(&now, 100));
        assert!(!changed.any(), "{from:?} to {to:?} made something stale");
        assert_eq!(look.font, font, "{from:?} to {to:?} refitted the face");
        assert_eq!(look.painted, painted);
    }
}

/// Restyling every window on a switch threw the effect stages away, so a
/// phosphor trail vanished and every pass redrew the window whole.
#[test]
fn a_light_dark_switch_keeps_the_effect_stages() {
    let (light, dark) = (Theme::light(), Theme::dark());
    let mut look = Look::resolve(&opaque_profile(FULL), &light, &desktop(&light, 100), ());
    let (grid, mut screen) = blank_row(&look);
    draw(&mut look, &grid, &mut screen);
    assert!(!look.refresh(&dark, &desktop(&dark, 100)).any());
    assert!(look.pipeline.retire(), "the stages are kept");
}

/// A new density re-sizes the face, so the grid is owed again and the stages
/// drawn over the old face are forgotten.
#[test]
fn a_new_scale_refits_the_face_and_forgets_the_stages() {
    let dark = Theme::dark();
    let mut look = Look::resolve(&opaque_profile(FULL), &dark, &desktop(&dark, 100), ());
    let (grid, mut screen) = blank_row(&look);
    draw(&mut look, &grid, &mut screen);
    let font = look.font;
    let changed = look.refresh(&dark, &desktop(&dark, 200));
    assert!(changed.metrics() && changed.repaints());
    assert!(!changed.painted(), "no colour is the desktop's");
    assert_ne!(look.font, font);
    assert_eq!(look.scale.percent(), 200);
    assert!(!look.pipeline.retire(), "the old face's stages went");
}

/// A new text size refits the face, and a later desktop change fits that size
/// rather than the one the window opened at.
#[test]
fn a_desktop_change_fits_the_text_size_the_profile_last_named() {
    let (light, dark) = (Theme::light(), Theme::dark());
    let on_light = desktop(&light, 100);
    let was = opaque_profile(0);
    let mut now = was;
    now.enlarge();
    let mut look = Look::resolve(&was, &light, &on_light, ());
    look.adopt(Invalidation::between(&was, &now), &now, &light, &on_light);
    let enlarged = Look::resolve(&now, &light, &on_light, ()).font;
    assert_eq!(look.font, enlarged);
    assert!(!look.refresh(&dark, &desktop(&dark, 100)).any());
    assert_eq!(look.font, enlarged);
}

/// An effect turned up keeps what the stages drew; new colours forget it, as
/// a trail of the old ones would ghost over them.
#[test]
fn an_effect_turned_up_keeps_the_stages_and_new_colours_forget_them() {
    let dark = Theme::dark();
    let on_dark = desktop(&dark, 100);
    let was = opaque_profile(FULL / 2);
    let mut look = Look::resolve(&was, &dark, &on_dark, ());
    let (grid, mut screen) = blank_row(&look);
    draw(&mut look, &grid, &mut screen);

    let stronger = opaque_profile(FULL);
    let changed = Invalidation::between(&was, &stronger);
    look.adopt(changed, &stronger, &dark, &on_dark);
    assert_eq!(look.effects, stronger.effects);
    assert!(look.pipeline.retire(), "a stronger pass keeps them");

    draw(&mut look, &grid, &mut screen);
    let recoloured = Profile {
        scheme: Scheme::Paper,
        ..stronger
    };
    let changed = Invalidation::between(&stronger, &recoloured);
    look.adopt(changed, &recoloured, &dark, &on_dark);
    assert_eq!(
        look.painted.scheme,
        Scheme::Paper.palette().expect("a fixed palette")
    );
    assert!(!look.pipeline.retire(), "new colours forget them");
}
