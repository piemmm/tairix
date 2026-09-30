//! Unit tests for what lies behind the column: the transparency a scene
//! shows through, the shadow the text over it carries, and the rectangle the
//! scene is kept clear of.

use alloc::vec;
use alloc::vec::Vec;

use tairix_font::TextShadow;
use tairix_geometry::{Point, Rect, Scale};
use tairix_input::{Key, NamedKey};
use tairix_raster::{Color, Pixel};
use tairix_theme::{Rgba, Theme};

use crate::layout::Prompt;
use crate::surface::{chrome_damage, text_shadow, AuthSurface, Backdrop};
use crate::testkit::{
    chrome, darkest_in, feed_in, key, named, over_scene, render_for_scene, render_in, still, theme,
    Scripted, SCREEN,
};
use crate::AccountTile;

/// A column clear of everything the surface centres, so what shows there is
/// the backdrop and nothing else.
const EDGE: u32 = 4;

/// The darkest ground a scene can have.
const NIGHT: Color = Color::rgb(0, 0, 0);

/// The test theme with its flat desktop colour set to `ground`.
fn grounded_on(ground: Color) -> Theme {
    let base = theme();
    let mut palette = *base.palette();
    palette.desktop = Rgba::rgb(ground.r, ground.g, ground.b);
    Theme::new(
        base.id(),
        base.name(),
        base.appearance(),
        palette,
        *base.metrics(),
        *base.fonts(),
        base.cursors().clone(),
        base.motion(),
        base.density(),
        base.contrast(),
    )
}

/// Behind the column a scene shows through untouched: nothing of the theme's
/// colour is laid over it.
#[test]
fn a_scene_shows_through_behind_the_column() {
    let surface = AuthSurface::new("ann", "ann");
    let frame = render_for_scene(&surface, NIGHT);
    for y in [1, SCREEN.height / 2, SCREEN.height - 2] {
        assert_eq!(
            frame.get(EDGE, y),
            Some(Pixel::TRANSPARENT),
            "row {y} of the backdrop was painted"
        );
    }
}

/// Over the scene's own ground the shadow composes to exactly what is already
/// there: a prompt and its chrome are, byte for byte, the flat frame painted in
/// that colour.
#[test]
fn over_its_own_ground_the_scene_frame_is_the_flat_one_in_that_colour() {
    let mut surface = AuthSurface::new("ann", "Ann Example");
    let _ = surface.set_chrome(chrome());
    let composed = over_scene(&render_for_scene(&surface, NIGHT), NIGHT);
    let flat = render_in(&surface, &grounded_on(NIGHT));
    let showed = composed
        .pixels()
        .iter()
        .zip(flat.pixels())
        .filter(|(scene, flat)| scene != flat)
        .count();
    assert_eq!(
        showed, 0,
        "a shadow in the ground's own colour showed on that ground"
    );
}

/// Text over a bright scene inks ground the plain draw would have left
/// showing, which is what keeps a pale ink readable where the light passes
/// behind it.
///
/// Measured over the whitest scene there is: in the account name's own band
/// nothing the surface draws — the scene, the ink, or the two blended — can be
/// darker than the ink itself, so a pixel that is proves the ground's shadow
/// landed behind the line.
#[test]
fn text_over_a_bright_scene_inks_ground_the_plain_draw_leaves_showing() {
    let surface = AuthSurface::new("Ann Example", "Ann Example");
    let ink = theme().palette().on_surface;

    let frame = over_scene(
        &render_for_scene(&surface, NIGHT),
        Color::rgb(255, 255, 255),
    );

    let darkest = darkest_in(&frame, Prompt::new(SCREEN, Scale::ONE).name);
    assert!(
        darkest < ink.g,
        "the name's band reached {darkest}, no darker than its own ink"
    );
}

/// The flat backdrop asks for no shadow at all, and a scene always asks for
/// one in its own ground. Over the flat colour there is nothing a shadow could
/// show against — it is the shadow's own colour — so the screen lock pays for
/// one glyph pass, not two.
#[test]
fn only_a_scene_asks_for_a_shadow_and_in_its_ground() {
    assert!(text_shadow(Scale::ONE, Backdrop::Desktop).is_none());
    assert!(
        text_shadow(Scale::ONE, Backdrop::Scene { ground: NIGHT }).is_some(),
        "a scene went unshadowed"
    );
}

/// Every pixel any stage of the column draws — the chooser, a picked
/// account's prompt, the typed-name prompt — stands inside the column's
/// rectangle, for one account and for enough to wrap the chooser, and the
/// chrome's inside the row a change to it repaints.
#[test]
fn everything_the_column_draws_stands_inside_its_rectangle() {
    let chrome_row = chrome_damage(SCREEN, Scale::ONE);
    for count in [1usize, 3, 9] {
        let accounts: Vec<AccountTile> = (0..count)
            .map(|at| AccountTile::new(&alloc::format!("User {at}"), &alloc::format!("u{at}")))
            .collect();
        let mut surface = AuthSurface::with_accounts(accounts);
        let _ = surface.set_chrome(chrome());
        let column = surface.column_rect(SCREEN, Scale::ONE);
        let quiet = still();
        let mut verifier = Scripted::refusing();
        let mut stages = vec![render_for_scene(&surface, NIGHT)];
        feed_in(
            &mut surface,
            &named(NamedKey::Enter),
            &mut verifier,
            0,
            &quiet,
        );
        stages.push(render_for_scene(&surface, NIGHT));
        for (stage, frame) in stages.iter().enumerate() {
            for y in 0..SCREEN.height {
                for x in 0..SCREEN.width {
                    let inked = frame.get(x, y).is_some_and(|pixel| pixel.a > 0);
                    let at = Point::new(
                        i32::try_from(x).expect("a small screen"),
                        i32::try_from(y).expect("a small screen"),
                    );
                    assert!(
                        !inked || column.contains(at) || chrome_row.contains(at),
                        "{count} accounts, stage {stage}: ({x}, {y}) outside {column:?}"
                    );
                }
            }
        }
    }
}

/// The rectangle is the column's, not the stage's: picking an account, typing,
/// and stepping back leave it exactly where it was.
#[test]
fn the_column_rectangle_is_the_same_whichever_stage_is_up() {
    let mut surface = AuthSurface::with_accounts(vec![
        AccountTile::new("Ann Example", "ann"),
        AccountTile::new("Bo Example", "bo"),
    ]);
    let before = surface.column_rect(SCREEN, Scale::ONE);
    let quiet = still();
    let mut verifier = Scripted::refusing();
    for event in [
        named(NamedKey::Enter),
        key(Key::Char('x')),
        named(NamedKey::Escape),
    ] {
        feed_in(&mut surface, &event, &mut verifier, 0, &quiet);
        assert_eq!(surface.column_rect(SCREEN, Scale::ONE), before);
    }
    assert!(
        before.width < SCREEN.width,
        "the column is narrower than the screen"
    );
    assert!(before.height < SCREEN.height);
}

/// A surface with no chooser — the screen lock's — stands in the prompt
/// alone, with room for its shadow and no more: the chrome along the top of
/// the screen is no part of it.
#[test]
fn a_lone_prompt_stands_in_the_prompt_alone() {
    let surface = AuthSurface::new("ann", "ann");
    let column = surface.column_rect(SCREEN, Scale::ONE);
    let prompt = Prompt::new(SCREEN, Scale::ONE);
    let shadow = TextShadow::new(Color::TRANSPARENT, Scale::ONE);
    let reach = i32::try_from(shadow.reach()).expect("a small reach");
    let drop = i32::try_from(shadow.drop()).expect("a small drop");
    assert_eq!(column.left(), prompt.block.left() - reach);
    assert_eq!(column.right(), prompt.block.right() + reach);
    assert_eq!(column.top(), prompt.disc.top() - reach);
    assert_eq!(column.bottom(), prompt.block.bottom() + reach + drop);
    assert!(column.top() > chrome_damage(SCREEN, Scale::ONE).bottom());
}

/// A screen too small for the chrome still has a column: the bodies'.
#[test]
fn a_screen_with_no_room_for_the_chrome_stands_in_the_bodies() {
    let small = Rect::new(0, 0, 480, 300);
    let surface = AuthSurface::with_accounts(vec![AccountTile::new("Ann Example", "ann")]);
    let column = surface.column_rect(small, Scale::ONE);
    assert!(!column.is_empty());
    assert!(column.top() >= small.top());
    assert!(column.bottom() <= small.bottom());
}
