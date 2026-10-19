//! The reference scene draws a real picture, and the same one every time and
//! at every size a window gives it.

use alloc::collections::BTreeSet;
use alloc::vec::Vec;

use tairix_raster::color::Pixel;
use tairix_raster::surface::Surface;
use tairix_reclaim::{GrowthAllowance, PressureBand, PressureGauge, Unpressured};
use tairix_wintersun_art::cache::MaterialCache;
use tairix_wintersun_figure::motion::Set;

use super::{cache, Discard, Owed, SceneAsk, World, CACHE_BACKING_BYTES, CAST_LEN};
use crate::error::ClientError;
use crate::frame::Renderer;
use crate::view::Reduced;

/// Window extents that take every path a window holding the scene can: the
/// smallest client the game declares, a desktop-sized one, and one past the
/// software path's cap, drawn reduced and resampled up.
const WINDOW_SIZES: [(u32, u32); 3] = [(320, 240), (1024, 768), (2600, 1500)];

/// Draw the scene into a window of `width`×`height` with `cache`, returning
/// its pixels and how many figures it drew.
fn window_frame(
    world: &mut World,
    width: u32,
    height: u32,
    cache: &mut MaterialCache,
) -> (Vec<Pixel>, usize) {
    let set = Set::new().expect("the shipped set");
    let clips = set.clips().expect("the shipped clips");
    let mut window = Surface::new(width, height).expect("a frame fits");
    let mut renderer = Renderer::new();
    world
        .draw_window(
            &clips,
            &mut window,
            &mut Reduced::default(),
            &mut renderer,
            cache,
            &tairix_parallel::SERIAL,
        )
        .expect("the scene draws");
    assert!(
        renderer.complete(),
        "the scene drew ground short of its detail"
    );
    (window.pixels().to_vec(), renderer.figures())
}

/// A gauge at critical pressure, which admits no tile at all.
struct Starved;

impl PressureGauge for Starved {
    fn sample(&self) -> PressureBand {
        PressureBand::Critical
    }

    fn growth_allowance(&self) -> GrowthAllowance {
        GrowthAllowance::refused()
    }
}

/// A drawing pressure cost a tile is refused rather than handed back in flat
/// tones as though it were the scene.
#[test]
fn a_drawing_the_cache_cost_a_tile_is_refused() {
    let mut world =
        World::generate(&tairix_parallel::SERIAL).expect("the reference realm generates");
    let set = Set::new().expect("the shipped set");
    let clips = set.clips().expect("the shipped clips");
    let mut window = Surface::new(320, 240).expect("a frame fits");
    let mut renderer = Renderer::new();
    let drawn = world.draw_window(
        &clips,
        &mut window,
        &mut Reduced::default(),
        &mut renderer,
        &mut cache(&Starved),
        &tairix_parallel::SERIAL,
    );
    assert_eq!(drawn, Err(ClientError::OutOfMemory));
    assert_ne!(renderer.refused_tiles(), 0, "the cache refused the tiles");
}

/// A picture that is stable but blank would prove nothing, which is the
/// failure a reproducibility check is most likely to have.
#[test]
fn every_window_size_draws_real_terrain_with_the_whole_cast() {
    let mut world =
        World::generate(&tairix_parallel::SERIAL).expect("the reference realm generates");
    for (width, height) in WINDOW_SIZES {
        let (pixels, figures) = window_frame(&mut world, width, height, &mut cache(&Unpressured));
        assert!(
            pixels.iter().all(|p| p.a == 255),
            "{width}x{height} left transparent pixels"
        );
        assert_eq!(figures, CAST_LEN, "{width}x{height} lost a figure");
        let colours = pixels
            .iter()
            .map(|p| (p.r, p.g, p.b))
            .collect::<BTreeSet<_>>();
        assert!(
            colours.len() > 16,
            "{width}x{height} is a flat fill of {} colours",
            colours.len()
        );
    }
}

/// The pinned cache never decides a pixel: a window drawn with it matches one
/// drawn with a cache sixteen times larger.
#[test]
fn the_pinned_cache_never_shapes_the_picture() {
    let mut world =
        World::generate(&tairix_parallel::SERIAL).expect("the reference realm generates");
    for (width, height) in WINDOW_SIZES {
        let (pinned, _) = window_frame(&mut world, width, height, &mut cache(&Unpressured));
        let mut ample = MaterialCache::new(
            "wintersun-reference-ample",
            CACHE_BACKING_BYTES * 16,
            &Unpressured,
            &Discard,
        );
        let (unbounded, _) = window_frame(&mut world, width, height, &mut ample);
        assert!(
            pinned == unbounded,
            "{width}x{height}: the pinned cache shaped the picture"
        );
    }
}

#[test]
fn the_scene_is_the_same_every_time_it_is_drawn() {
    let mut world =
        World::generate(&tairix_parallel::SERIAL).expect("the reference realm generates");
    let (first, _) = window_frame(&mut world, 1024, 768, &mut cache(&Unpressured));
    let mut again =
        World::generate(&tairix_parallel::SERIAL).expect("the reference realm generates");
    let (second, _) = window_frame(&mut again, 1024, 768, &mut cache(&Unpressured));
    assert!(
        first == second,
        "two drawings of the reference scene differ"
    );
}

#[test]
fn a_resize_asks_once_for_its_extent() {
    let (small, large) = ((640, 360), (1280, 720));
    let frame = |extent| {
        Some(SceneAsk {
            extent: Some(extent),
            give_back: false,
        })
    };
    let mut owed = Owed::new();
    assert_eq!(owed.ask(Some(small), None), frame(small));
    assert_eq!(
        owed.ask(Some(small), None),
        None,
        "asked twice for one extent"
    );
    assert!(owed.landed(small, Some(small)));
    assert_eq!(
        owed.ask(Some(small), Some(small)),
        None,
        "asked again for the frame on screen"
    );
    assert_eq!(owed.ask(Some(large), Some(small)), frame(large));
    assert_eq!(
        owed.ask(None, Some(small)),
        None,
        "a window with no extent owes nothing"
    );
}

#[test]
fn a_frame_for_an_extent_the_window_left_is_not_shown() {
    let (first, second) = ((640, 360), (800, 600));
    let mut owed = Owed::new();
    assert!(owed.ask(Some(first), None).is_some());
    assert!(owed.ask(Some(second), None).is_some());
    assert!(!owed.landed(first, Some(second)), "a stale frame was shown");
    assert_eq!(
        owed.ask(Some(second), None),
        None,
        "the newest ask still stands"
    );
    assert!(owed.landed(second, Some(second)));
}

#[test]
fn a_give_back_waits_for_the_frame_being_drawn() {
    let extent = (640, 360);
    let mut owed = Owed::new();
    assert!(owed.ask(Some(extent), None).is_some());
    owed.pressure_moved();
    assert_eq!(
        owed.ask(Some(extent), None),
        None,
        "the give-back displaced the frame in flight"
    );
    assert!(owed.landed(extent, Some(extent)));
    assert_eq!(
        owed.ask(Some(extent), Some(extent)),
        Some(SceneAsk {
            extent: None,
            give_back: true
        })
    );
    assert_eq!(
        owed.ask(Some(extent), Some(extent)),
        None,
        "a give-back is asked for once"
    );
}

#[test]
fn a_give_back_rides_with_the_next_frame_asked() {
    let (small, large) = ((640, 360), (1280, 720));
    let mut owed = Owed::new();
    assert!(owed.ask(Some(small), None).is_some());
    owed.pressure_moved();
    assert_eq!(
        owed.ask(Some(large), None),
        Some(SceneAsk {
            extent: Some(large),
            give_back: true
        })
    );
    assert!(owed.landed(large, Some(large)));
    assert_eq!(owed.ask(Some(large), Some(large)), None);
}
