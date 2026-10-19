//! What the light does to a surface: the sun across its slope, the air
//! between it and the eye, the sky where nothing stands, and the silhouette
//! a surface's edge leaves against what lies beyond it.

use tairix_raster::color::{Color, Pixel};
use tairix_raster::mesh::{Depth, EdgeNear, Toward};
use tairix_raster::surface::Surface;
use tairix_raster::view::Perspective;

use super::{Atmosphere, Sky, Sun, MIST_CEILING, UNSHADED};
use crate::raster::{GBuffer, Texel};
use crate::terrain::VIEW_RANGE;

const WIDTH: u32 = 32;
const HEIGHT: u32 = 24;

/// A camera looking north and gently down, so the sky fills the top of the
/// screen and the ground the bottom.
fn camera() -> Perspective {
    Perspective::new(
        [0.0, 0.0, 10_000.0],
        [0.0, -0.98, -0.2],
        [1.0, 0.0, 0.0],
        [0.0, 0.2, -0.98],
        30.0,
        (WIDTH, HEIGHT),
        256.0,
    )
    .expect("a camera")
}

fn atmosphere(sun: &Sun, lit: bool) -> Atmosphere {
    Atmosphere::new(sun, Sky::daylight(), &camera(), HEIGHT, lit).expect("room")
}

/// A grey surface on high ground just ahead, out of the mist and the fog.
fn grey(normal: [i8; 2]) -> (Depth, Pixel, Texel) {
    (
        Depth(u32::MAX / 2),
        Pixel {
            r: 120,
            g: 120,
            b: 120,
            a: 255,
        },
        Texel {
            normal,
            elevation: i16::try_from(MIST_CEILING + 1).expect("an elevation"),
        },
    )
}

/// The light `atmosphere` gives the surface `(depth, albedo, texel)`.
fn lit(atmosphere: &Atmosphere, (depth, albedo, texel): (Depth, Pixel, Texel)) -> Pixel {
    atmosphere.shade(depth, albedo, texel, 0)
}

fn brightness(pixel: Pixel) -> u32 {
    u32::from(pixel.r) + u32::from(pixel.g) + u32::from(pixel.b)
}

#[test]
fn level_ground_keeps_its_palette_colour() {
    let sun = Sun::daylight();
    assert_eq!(sun.gain(UNSHADED), Color::rgb(UNSHADED, UNSHADED, UNSHADED));
    let atmosphere = atmosphere(&sun, true);
    let flat = lit(&atmosphere, grey([0, 0]));
    for channel in [flat.r, flat.g, flat.b] {
        assert!(channel.abs_diff(120) <= 2, "{flat:?}");
    }
}

#[test]
fn a_slope_toward_the_sun_brightens_and_one_away_darkens() {
    let sun = Sun::daylight();
    let shaded = atmosphere(&sun, true);
    // The sun travels south-east, so it shines from the north-west: a slope
    // whose normal leans north-west faces it.
    let toward = lit(&shaded, grey([-40, -30]));
    let away = lit(&shaded, grey([40, 30]));
    let flat = lit(&shaded, grey([0, 0]));
    assert!(
        brightness(toward) > brightness(flat),
        "{toward:?} against {flat:?}"
    );
    assert!(
        brightness(away) < brightness(flat),
        "{away:?} against {flat:?}"
    );
    // Unlit, slope or no slope, a surface is its palette colour.
    let unlit = atmosphere(&sun, false);
    assert_eq!(lit(&unlit, grey([-40, -30])), lit(&unlit, grey([0, 0])));
}

#[test]
fn the_shade_saturates_past_the_steepest_walkable_slope() {
    let atmosphere = atmosphere(&Sun::daylight(), true);
    // Both far steeper than a step a body may climb, both facing away.
    let steep = lit(&atmosphere, grey([100, 75]));
    let steeper = lit(&atmosphere, grey([110, 82]));
    assert_eq!(steep, steeper);
}

#[test]
fn a_sun_overhead_shades_no_slope() {
    let overhead = Sun::new(
        0,
        0,
        Color::rgb(255, 240, 216),
        Color::rgb(140, 152, 178),
        200,
    );
    let atmosphere = atmosphere(&overhead, true);
    assert_eq!(
        lit(&atmosphere, grey([60, -20])),
        lit(&atmosphere, grey([0, 0]))
    );
}

#[test]
fn mist_pools_in_hollows_and_thins_on_high_ground() {
    let atmosphere = atmosphere(&Sun::daylight(), true);
    let sky = Sky::daylight();
    assert_eq!(atmosphere.mist(0), sky.mist_depth);
    let ceiling = i16::try_from(MIST_CEILING).expect("an elevation");
    assert_eq!(atmosphere.mist(ceiling), 0);
    assert_eq!(atmosphere.mist(i16::MAX), 0);
    assert!(atmosphere.mist(ceiling / 2) < sky.mist_depth);
    assert!(atmosphere.mist(ceiling / 2) > 0);
}

#[test]
fn the_fog_closes_the_view_with_distance() {
    let view = camera();
    let atmosphere = atmosphere(&Sun::daylight(), true);
    let at = |distance: f64| {
        let depth = view.depth(distance);
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a depth in range, rounded down"
        )]
        let depth = Depth(depth as u32);
        atmosphere.fog(depth)
    };
    assert!(at(1_000.0) <= 1, "fog right in front of the eye");
    let range = f64::from(VIEW_RANGE);
    assert!(
        at(range) >= 240,
        "the view range is not where the fog closes it"
    );
    let mut last = 0;
    for step in 1..40 {
        let fog = at(range * f64::from(step) / 40.0);
        assert!(fog >= last, "the fog thinned with distance");
        last = fog;
    }
    assert_eq!(atmosphere.fog(Depth::CLEAR), u8::MAX);
}

#[test]
fn a_figures_veil_is_the_mist_and_the_fog_it_stands_in() {
    let atmosphere = atmosphere(&Sun::daylight(), true);
    let far = Depth(2_000_000);
    let (mist, fog) = (atmosphere.mist(0), atmosphere.fog(far));
    let veil = atmosphere.veil(far, 0);
    assert!(veil >= mist && veil >= fog);
    assert_eq!(
        atmosphere.veil(Depth(u32::MAX), i16::MAX),
        atmosphere.fog(Depth(u32::MAX))
    );
    assert_eq!(atmosphere.veil_tone(), Sky::daylight().horizon);
}

#[test]
fn the_figures_are_lit_by_the_sun_the_harness_measures_them_under() {
    assert_eq!(
        Sun::daylight().light(),
        Ok(tairix_wintersun_figure::reference::Reference::light().expect("the harness sun")),
        "the game draws its figures under a light the art harness never measured"
    );
    let overhead = Sun::new(0, 0, Color::rgb(0, 0, 0), Color::rgb(0, 0, 0), 0);
    assert!(
        overhead.light().is_ok(),
        "an overhead sun left the figures unlit"
    );
}

/// The frame `buffer` composites to.
fn composited(atmosphere: &Atmosphere, buffer: &GBuffer) -> Surface {
    let mut target = Surface::new(WIDTH, HEIGHT).expect("a surface");
    for mut band in target.row_bands_mut(0..HEIGHT, HEIGHT) {
        atmosphere.composite(buffer, &mut band);
    }
    target
}

#[test]
fn where_nothing_stands_the_sky_shows_bluer_overhead() {
    let atmosphere = atmosphere(&Sun::daylight(), true);
    let mut buffer = GBuffer::new();
    buffer.reset(WIDTH, HEIGHT).expect("room");
    let frame = composited(&atmosphere, &buffer);
    let (top, bottom) = (
        frame.get(WIDTH / 2, 0).expect("a pixel"),
        frame.get(WIDTH / 2, HEIGHT - 1).expect("a pixel"),
    );
    assert!(top.b > top.r, "the sky overhead is not blue: {top:?}");
    assert!(
        i32::from(top.b) - i32::from(top.r) > i32::from(bottom.b) - i32::from(bottom.r),
        "the sky is no bluer overhead than at the horizon"
    );
    assert!(frame.pixels().iter().all(|pixel| pixel.a == u8::MAX));
}

/// A surface's silhouette against the sky takes the sky's share of the
/// pixel beyond its edge, and one against more of the same surface does not.
#[test]
fn a_silhouette_takes_the_share_of_whatever_lies_beyond_it() {
    let atmosphere = atmosphere(&Sun::daylight(), true);
    let (depth, albedo, texel) = grey([0, 0]);
    let quarter = EdgeNear::new(EdgeNear::STEPS / 4, Toward::Up);
    let mut buffer = GBuffer::new();
    buffer.reset(WIDTH, HEIGHT).expect("room");
    // Ground from row 10 down; row 10's edge lies a quarter pixel up, toward
    // the sky in row 9.
    buffer.paint_row(10, 0, depth, albedo, texel, quarter);
    for row in 11..HEIGHT {
        buffer.paint_row(row, 0, depth, albedo, texel, EdgeNear::NONE);
    }
    let frame = composited(&atmosphere, &buffer);
    let ground = frame.get(4, 14).expect("a pixel");
    let sky = frame.get(4, 9).expect("a pixel");
    let edge = frame.get(4, 10).expect("a pixel");
    assert_ne!(edge, ground, "the silhouette was not resolved");
    let between = |a: u8, b: u8, c: u8| (a.min(b)..=a.max(b)).contains(&c);
    assert!(between(ground.r, sky.r, edge.r) && between(ground.b, sky.b, edge.b));

    // The same record with the same surface beyond it is an inner edge.
    buffer.paint_row(9, 0, depth, albedo, texel, EdgeNear::NONE);
    let frame = composited(&atmosphere, &buffer);
    assert_eq!(frame.get(4, 10), Some(ground));
}

/// A buffer drawn again at the same size keeps last frame's records beside
/// its cleared depth, and where nothing is drawn now none of them is read.
#[test]
fn ground_drawn_last_frame_leaves_nothing_where_sky_shows_now() {
    let atmosphere = atmosphere(&Sun::daylight(), true);
    let (depth, albedo, texel) = grey([0, 0]);
    let toward_ground = EdgeNear::new(EdgeNear::STEPS / 4, Toward::Down);
    let mut reused = GBuffer::new();
    reused.reset(WIDTH, HEIGHT).expect("room");
    for row in 8..HEIGHT {
        reused.paint_row(row, 0, depth, albedo, texel, toward_ground);
    }
    let _ = composited(&atmosphere, &reused);
    let paint = |buffer: &mut GBuffer| {
        buffer.reset(WIDTH, HEIGHT).expect("room");
        for row in 12..HEIGHT {
            buffer.paint_row(row, 0, depth, albedo, texel, EdgeNear::NONE);
        }
    };
    paint(&mut reused);
    let mut fresh = GBuffer::new();
    paint(&mut fresh);
    assert_eq!(
        composited(&atmosphere, &reused).pixels(),
        composited(&atmosphere, &fresh).pixels(),
        "the picture depends on what the buffer held before"
    );
}
