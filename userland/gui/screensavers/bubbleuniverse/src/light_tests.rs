//! Host tests of the light: a dot's weight wherever it falls, a strip's
//! spots, how finely a spot holds its light, and a paint that covers exactly
//! its square, the same in bands of whole strips, within its share of memory.

extern crate std;

use alloc::vec::Vec;

use tairix_geometry::{Point, Rect};
use tairix_parallel::{JobRunner, Reversed, Threaded, SERIAL};
use tairix_raster::{Color, Surface};
use tairix_rng::{NonCryptoRng, RandU64};

use super::{
    splat, tent, Bins, Dot, Light, Placed, Shed, Spot, MAX_RADIUS, SCRATCH_SHARE, STRIP_ROWS, SUB,
};
use crate::glow::Glow;
use crate::tone::{Tone, LIGHT_ONE};

/// `count` spots scattered over `area`, in 256ths of a pixel, the same every
/// run.
fn scattered(seed: u64, count: usize, area: Rect) -> Vec<Spot> {
    let mut rng = NonCryptoRng::seed_from_u64(seed);
    let mut below = |bound: u32| u32::try_from(rng.next_below(u64::from(bound))).expect("below");
    let sub = SUB.unsigned_abs();
    (0..count)
        .map(|_| Spot {
            x: area.left() * SUB + i32::try_from(below(area.width * sub)).expect("small"),
            y: area.top() * SUB + i32::try_from(below(area.height * sub)).expect("small"),
            light: [400 + below(3000), below(2000), below(4000)]
                .map(|l| u16::try_from(l).expect("small")),
        })
        .collect()
}

/// The weights a tent `TAPS` across lays along an axis centred `at`, summed.
fn weight<const TAPS: usize>(at: i32) -> u32 {
    tent::<TAPS>(at).1.iter().sum()
}

/// A dot's taps along an axis weigh the same in all wherever its centre
/// falls between pixels, so a point moving across the screen holds its
/// brightness; and the first pixel the tent lights is the dot's.
#[test]
fn a_dot_weighs_the_same_wherever_it_falls() {
    for at in (1000 * SUB..1001 * SUB).step_by(3) {
        assert_eq!(weight::<2>(at), 256, "{at}");
        assert_eq!(weight::<4>(at), 256 * 4, "{at}");
        assert_eq!(weight::<6>(at), 256 * 9, "{at}");
        assert_eq!(weight::<8>(at), 256 * 16, "{at}");
        assert_eq!(tent::<2>(at).0, Dot::new(1).first(at));
        assert_eq!(tent::<8>(at).0, Dot::new(4).first(at));
    }
    assert_eq!(Dot::new(0).radius(), 1);
    assert_eq!(Dot::new(99).radius(), MAX_RADIUS);
}

/// A dot drawn whole sheds its light times its energy, to the rounding of its
/// taps, a light held finer shedding that much less, and touches no channel
/// it carries no light in. Its brightest tap stays short of what a channel
/// holds, so nothing is lost to saturation.
#[test]
fn a_splat_sheds_the_dot_s_energy() {
    fn shed<const TAPS: usize>(radius: u32, shift: u32) {
        let mut light = alloc::vec![[0u16; 3]; 64 * 64];
        let spot = Spot {
            x: 30 * SUB + 77,
            y: 31 * SUB + 200,
            light: [4_000, 0, 20],
        };
        splat::<TAPS>(&spot, (0, 0), (64, shift), &mut light);
        let shed: u64 = light.iter().map(|cell| u64::from(cell[0])).sum();
        let expected = (4_000 * Dot::new(radius).energy()) >> shift;
        let taps = u64::from(4 * radius * radius);
        assert!(
            shed.abs_diff(expected) <= taps,
            "radius {radius} held {shift} finer: {shed} against {expected}"
        );
        assert!(
            light.iter().all(|cell| cell[1] == 0),
            "no light where none was carried"
        );
    }
    for shift in [0, 4, 9] {
        shed::<2>(1, shift);
        shed::<4>(2, shift);
        shed::<6>(3, shift);
        shed::<8>(4, shift);
    }
}

/// A dot drawn where its rows or columns would pass the light it is drawn
/// into is not drawn at all, rather than wrapping onto another row or being
/// cut short.
#[test]
fn a_splat_past_its_light_is_not_drawn() {
    let mut light = alloc::vec![[0u16; 3]; 16 * 16];
    for (x, y) in [
        (15 * SUB + 200, 8 * SUB),
        (-SUB, 8 * SUB),
        (8 * SUB, -SUB),
        (8 * SUB, 40 * SUB),
        (8 * SUB, 15 * SUB + 230),
    ] {
        splat::<2>(
            &Spot {
                x,
                y,
                light: [500; 3],
            },
            (0, 0),
            (16, 0),
            &mut light,
        );
    }
    assert!(light.iter().all(|cell| *cell == [0; 3]));
}

/// A spot's full strength is held in all sixteen bits of its light however
/// little light its dot sheds, and every strength comes back as the light it
/// is worth.
#[test]
fn a_spot_holds_its_light_as_finely_as_sixteen_bits_allow() {
    for peak in [2.0, 0.5, 1.0 / 32.0, 1e-4] {
        let shed = Shed::new(peak);
        let full = shed.carried(1.0);
        assert!(full >= 1 << 15, "{peak}: {full}");
        for strength in [1.0, 0.25, 0.003_67, 0.000_3] {
            let worth = strength * peak * f64::from(LIGHT_ONE);
            let held = f64::from(shed.carried(strength)) / f64::from(1u32 << shed.shift());
            assert!(
                (held - worth).abs() <= 1.0 / f64::from(1u32 << shed.shift()),
                "{peak} at {strength}: {held} against {worth}"
            );
            assert!(shed.carried(strength) > 0, "{peak} at {strength} is lit");
        }
    }
}

/// Every spot is listed in each strip its dot reaches and in no other.
#[test]
fn a_strip_holds_exactly_the_spots_that_reach_it() {
    let painted = Rect::new(37, 21, 300, 211);
    let dot = Dot::new(2);
    let mut spots = scattered(7, 5_000, Rect::new(30, 10, 320, 240));
    spots.push(Spot {
        x: 100 * SUB,
        y: (painted.top() - 40) * SUB,
        light: [1; 3],
    });
    let bins = binned(&spots, 7, painted, dot);
    let count = usize::try_from(painted.height.div_ceil(STRIP_ROWS.unsigned_abs())).expect("small");
    assert_eq!(bins.layout.count, count);
    for strip in 0..count {
        let rows = bins.layout.rows(strip);
        let expected: Vec<Spot> = spots
            .iter()
            .filter(|spot| {
                let reach = dot.covers(spot.y);
                reach.start < rows.end && reach.end > rows.start
            })
            .copied()
            .collect();
        let listed: Vec<Spot> = bins.spots(strip).flatten().copied().collect();
        assert_eq!(
            listed, expected,
            "strip {strip}: every spot, in the order traced"
        );
    }
}

/// `spots` sorted into bins by `jobs` jobs, each its own run of them, over
/// the `painted` rows.
fn binned(spots: &[Spot], jobs: usize, painted: Rect, dot: Dot) -> Bins {
    let per_job = spots.len().div_ceil(jobs);
    let mut bins = Bins::new(jobs, per_job, 2_000, dot).expect("bins");
    for (mut share, traced) in bins.shares(painted).zip(spots.chunks(per_job)) {
        share.sort(traced);
    }
    bins
}

/// The light of a `side`-pixel frame of dots `radius` pixels each way, with
/// its glow if it glows, holding `spots` sorted by `jobs` jobs over the
/// `painted` rows, its scratch held to a share of `heap` bytes.
fn light(
    (side, radius, glowing): (u32, u32, bool),
    (spots, jobs, painted): (&[Spot], usize, Rect),
    (runner, heap): (&dyn JobRunner, u64),
) -> Light {
    let dot = Dot::new(radius);
    // The reference screen's: a light held four bits past light's own.
    let shed = Shed::new(0.5);
    let glow =
        glowing.then(|| Glow::new((side, side), 4, (dot.energy(), shed), runner).expect("a glow"));
    let tone = Tone::new(crate::EXPOSURE).expect("a tone");
    let per_job = spots.len().div_ceil(jobs).max(1);
    let bins = Bins::new(jobs, per_job, 2_000, dot).expect("bins");
    let mut light =
        Light::new((side, side), (bins, shed), (tone, glow), (runner, heap)).expect("a light");
    for (mut share, traced) in light.shares(painted).zip(spots.chunks(per_job)) {
        share.sort(traced);
    }
    light
}

/// What a surface holds where no paint has reached: no paint writes a clear
/// pixel.
const MARK: Color = Color::TRANSPARENT;

/// A paint writes every pixel of the painted part of its square opaque, and
/// nothing beyond it, the screen's edge clipping the square, at every size of
/// dot.
#[test]
fn a_paint_covers_its_square_on_the_screen_and_nothing_else() {
    let frame = Rect::new(-30, 40, 280, 280);
    let painted = frame.intersection(&Rect::new(0, 0, 400, 300));
    let spots = scattered(3, 4_000, Rect::new(20, 80, 200, 200));
    let mark = MARK.premultiply();
    for radius in 1..=MAX_RADIUS {
        let mut surface = Surface::new(400, 300).expect("a surface");
        surface.fill(MARK);
        let mut light = light(
            (280, radius, true),
            (&spots, 5, painted),
            (&SERIAL, u64::MAX),
        );
        light.paint(&spots, &Placed { frame, painted }, &mut surface, &SERIAL);
        let mut lit = 0;
        for y in 0..300 {
            for x in 0..400 {
                let pixel = surface.get(x, y).expect("inside");
                let at = Point::new(
                    i32::try_from(x).expect("small"),
                    i32::try_from(y).expect("small"),
                );
                if painted.contains(at) {
                    assert_eq!(pixel.a, u8::MAX, "radius {radius}: ({x}, {y}) painted");
                    lit += usize::from(pixel.r > 0 || pixel.g > 0 || pixel.b > 0);
                } else {
                    assert_eq!(pixel, mark, "radius {radius}: ({x}, {y}) untouched");
                }
            }
        }
        assert!(lit > 2_000, "radius {radius}: the spots are drawn");
    }
}

/// A paint split into bands of rows across cores, whichever band runs first
/// and when they run at once, is the paint drawn on one, pixel for pixel, at
/// every size of dot.
#[test]
fn a_paint_in_bands_is_the_paint_drawn_whole() {
    let frame = Rect::new(10, 5, 330, 330);
    let screen = Rect::new(0, 0, 360, 320);
    let placed = Placed {
        frame,
        painted: frame.intersection(&screen),
    };
    let spots = scattered(11, 20_000, Rect::new(40, 30, 270, 270));
    let sorted = (spots.as_slice(), 9, placed.painted);
    for radius in 1..=MAX_RADIUS {
        for glowing in [true, false] {
            let size = (330, radius, glowing);
            let mut whole = Surface::new(360, 320).expect("a surface");
            light(size, sorted, (&SERIAL, u64::MAX)).paint(&spots, &placed, &mut whole, &SERIAL);
            let reversed = Reversed::new(4);
            let threaded = Threaded::new(4);
            let runners: [&dyn JobRunner; 2] = [&reversed, &threaded];
            for runner in runners {
                let mut banded = Surface::new(360, 320).expect("a surface");
                light(size, sorted, (runner, u64::MAX)).paint(&spots, &placed, &mut banded, runner);
                assert!(
                    banded == whole,
                    "radius {radius} glowing {glowing}: the bands drew the paint"
                );
            }
            if !glowing {
                assert!(
                    reversed.widest() > 1,
                    "radius {radius}: the paint was split"
                );
            }
        }
    }
}

/// A paint cuts its rows into bands of whole strips, counted from the first
/// row it paints, so no strip's dots are drawn twice.
#[test]
fn a_paint_s_bands_hold_whole_strips() {
    let runner = Reversed::new(8);
    let painted = Rect::new(10, 7, 330, 301);
    let light = light((330, 1, false), (&[], 9, painted), (&runner, u64::MAX));
    let mut surface = Surface::new(360, 320).expect("a surface");
    let (top, bottom) = (
        painted.top().unsigned_abs(),
        painted.bottom().unsigned_abs(),
    );
    let mut cut = Vec::new();
    surface.paint_bands(
        top..bottom,
        &runner,
        light.split(),
        |band| cut.push(band.rows()),
        &|(): &mut ()| {},
    );
    assert!(cut.len() > 1, "{cut:?}");
    let strip = STRIP_ROWS.unsigned_abs();
    for band in cut.iter().take(cut.len() - 1) {
        assert_eq!((band.start - top) % strip, 0, "{band:?} starts a strip");
        assert_eq!((band.end - top) % strip, 0, "{band:?} ends one");
    }
}

/// However many cores a large frame is painted across, its scratch holds no
/// more than its share of the heap, so the saver starts where a band for every
/// core would not fit; a heap that holds them all gets them all.
#[test]
fn a_paint_s_scratch_is_held_to_a_share_of_the_heap() {
    let wide = Reversed::new(64);
    let held = |side: u32, heap: u64| {
        let painted = Rect::new(0, 0, side, side);
        let light = light((side, 2, false), (&[], 4, painted), (&wide, heap));
        let bytes: usize = light
            .scratch
            .iter()
            .map(|scratch| {
                let glow = &scratch.glow;
                scratch.light.len() + glow.cells.len() + glow.rows[0].len() + glow.rows[1].len()
            })
            .sum();
        (light.scratch.len(), bytes * size_of::<[u16; 3]>())
    };
    let heap = 16 << 20;
    let (bands, bytes) = held(2868, heap);
    assert!(bands > 1, "still split");
    assert!(
        u64::try_from(bytes).expect("small") <= heap / SCRATCH_SHARE,
        "{bands} scratches of {bytes} bytes"
    );
    let (all, _) = held(330, heap);
    assert_eq!(all, tairix_parallel::bands(&wide, 330usize.div_ceil(16), 1));
}
