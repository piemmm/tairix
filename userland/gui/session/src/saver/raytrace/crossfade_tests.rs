//! Host tests of a change crossfaded in: from what the screen showed to the
//! new picture, writing the change's tiles and nothing else, the same however
//! the rows are shared across cores, and wanting each weight once as the
//! clock runs on.

use tairix_abi::time::NANOS_PER_MILLI as MS;
use tairix_parallel::{JobRunner, Reversed, Threaded, SERIAL};
use tairix_raster::Pixel;
use tairix_theme::Timeline;
use tairix_wm::{Color, Rect, Region, Surface};

use super::Crossfade;
use crate::saver::raytrace::tiles::Tiles;

/// The colour pixel `(x, y)` of a picture under `seed` shows: different from
/// its neighbours.
fn speckle(x: u32, y: u32, seed: u32) -> Pixel {
    let level = |channel: u32| {
        let hashed = (x.wrapping_mul(0x9e37_79b9) ^ y.wrapping_mul(0x85eb_ca6b) ^ seed ^ channel)
            .wrapping_mul(0x2c1b_3c6d);
        u8::try_from(hashed >> 24).expect("a byte")
    };
    Pixel {
        r: level(1),
        g: level(2),
        b: level(3),
        a: u8::MAX,
    }
}

/// A `size` picture filled with `fill`.
fn filled(size: (u32, u32), fill: Color) -> Surface {
    let mut surface = Surface::new(size.0, size.1).expect("a surface");
    surface.fill(fill);
    surface
}

/// The tiles of a `size` picture that `rects` touch, and their cover.
fn change(size: (u32, u32), rects: &[Rect]) -> (Tiles, Region) {
    let mut tiles = Tiles::new(size).expect("tiles");
    let mut room = Tiles::new(size).expect("room");
    for rect in rects {
        tiles.mark(*rect);
    }
    let mut cover = Region::new();
    tiles.cover(&mut room, &mut cover);
    (tiles, cover)
}

/// Whether pixel `(x, y)` lies in one of `tiles`' runs.
fn within(tiles: &Tiles, x: u32, y: u32) -> bool {
    tiles
        .spans(0..tiles.rows())
        .any(|(columns, lines)| columns.contains(&x) && lines.contains(&y))
}

/// A crossfade over `size` beginning, at `now_ns` and over `span_ms`, to fade
/// in `tiles`' change, its new picture `seed`'s speckle wherever `paint`
/// says.
fn begun(
    size: (u32, u32),
    (tiles, cover): (&Tiles, &mut Region),
    (now_ns, span_ms): (u64, u16),
    (seed, paint): (u32, &dyn Fn(u32, u32) -> bool),
) -> Crossfade {
    let mut fade = Crossfade::new(size).expect("a crossfade");
    let picture = fade.begin((tiles, cover), Timeline::start(now_ns, span_ms), &SERIAL);
    for y in 0..size.1 {
        for x in 0..size.0 {
            if paint(x, y) {
                picture.set(x, y, speckle(x, y, seed));
            }
        }
    }
    fade
}

/// A fade's ends are what the screen showed and the new picture, and between
/// them each pixel lies within a level of the straight mix of the two.
#[test]
fn a_fade_runs_from_what_the_screen_showed_to_the_new_picture() {
    let size = (100u32, 60u32);
    let (tiles, mut cover) = change(size, &[Rect::new(20, 10, 40, 20)]);
    let fade = begun(
        size,
        (&tiles, &mut cover),
        (0, 500),
        (7, &|x, y| within(&tiles, x, y)),
    );
    let black = filled(size, Color::rgb(0, 0, 0));
    let mut screen = black.clone();
    fade.draw(&mut screen, 0, &SERIAL);
    assert_eq!(screen, black, "nothing of the change at first");
    fade.draw(&mut screen, u8::MAX, &SERIAL);
    for y in 0..size.1 {
        for x in 0..size.0 {
            let expected = if within(&tiles, x, y) {
                speckle(x, y, 7)
            } else {
                black.get(x, y).expect("a pixel")
            };
            assert_eq!(screen.get(x, y), Some(expected), "({x}, {y}) once whole");
        }
    }
    fade.draw(&mut screen, 128, &SERIAL);
    for y in 0..size.1 {
        for x in 0..size.0 {
            let shown = screen.get(x, y).expect("a pixel");
            let whole = if within(&tiles, x, y) {
                speckle(x, y, 7)
            } else {
                Color::rgb(0, 0, 0).premultiply()
            };
            for (level, end) in [(shown.r, whole.r), (shown.g, whole.g), (shown.b, whole.b)] {
                let half = u32::from(end) * 128 / 255;
                assert!(
                    u32::from(level).abs_diff(half) <= 1,
                    "({x}, {y}) half way: {level} where the mix is {half}"
                );
            }
        }
    }
}

/// A frame writes the change's tiles alone, however the new picture differs
/// from the screen beyond them.
#[test]
fn a_fade_writes_nothing_beyond_its_tiles() {
    let size = (100u32, 60u32);
    let (tiles, mut cover) = change(size, &[Rect::new(70, 40, 5, 5), Rect::new(3, 3, 1, 1)]);
    let fade = begun(size, (&tiles, &mut cover), (0, 500), (9, &|_, _| true));
    let ground = filled(size, Color::rgb(40, 90, 140));
    for weight in [1u8, 100, u8::MAX] {
        let mut screen = ground.clone();
        fade.draw(&mut screen, weight, &SERIAL);
        for y in 0..size.1 {
            for x in 0..size.0 {
                if !within(&tiles, x, y) {
                    assert_eq!(screen.get(x, y), ground.get(x, y), "({x}, {y}) at {weight}");
                }
            }
        }
        assert_ne!(screen, ground, "the tiles change at {weight}");
    }
    let marked: u64 = fade
        .cover
        .rects()
        .iter()
        .map(|rect| u64::from(rect.width) * u64::from(rect.height))
        .sum();
    assert_eq!(marked, 2 * 16 * 16, "the cover is the two tiles");
}

/// However its rows are shared across cores, and whichever share runs first,
/// a frame is the frame one core draws.
#[test]
fn drawing_across_cores_draws_what_one_core_draws() {
    let size = (640u32, 360u32);
    let rects: alloc::vec::Vec<Rect> = (0..200i32)
        .map(|n| Rect::new((n * 131) % 640, (n * 71) % 360, 9, 9))
        .collect();
    let (tiles, mut cover) = change(size, &rects);
    let fade = begun(size, (&tiles, &mut cover), (0, 500), (3, &|_, _| true));
    let pool = Threaded::new(4);
    let backwards = Reversed::new(4);
    let runners: [&dyn JobRunner; 3] = [&SERIAL, &pool, &backwards];
    let ground = filled(size, Color::rgb(0, 0, 0));
    let screens = runners.map(|runner| {
        let mut screen = ground.clone();
        fade.draw(&mut screen, 77, runner);
        screen
    });
    assert_eq!(screens[0], screens[1]);
    assert_eq!(screens[0], screens[2]);
    assert!(backwards.widest() > 1, "the frame was shared out");
}

/// As the clock runs a fade wants each new weight once, and nothing once the
/// whole change is shown.
#[test]
fn a_fade_wants_each_weight_once_and_nothing_once_whole() {
    let size = (32u32, 32u32);
    let (tiles, mut cover) = change(size, &[Rect::new(0, 0, 8, 8)]);
    let mut fade = begun(
        size,
        (&tiles, &mut cover),
        (100 * MS, 500),
        (1, &|_, _| false),
    );
    assert!(!fade.settled());
    assert_eq!(fade.due(100 * MS), None, "nothing new at its start");
    assert_eq!(fade.due(350 * MS), Some(127));
    fade.shown(127);
    assert_eq!(
        fade.due(350 * MS),
        None,
        "a weight shown is not wanted again"
    );
    assert_eq!(fade.due(600 * MS), Some(u8::MAX));
    fade.shown(u8::MAX);
    assert!(fade.settled());
    assert_eq!(fade.due(700 * MS), None);
}
