//! Host tests of the light: a dot's weight wherever it falls, a strip's
//! spots, and a paint that covers exactly its square, the same in bands.

extern crate std;

use alloc::vec::Vec;

use tairix_geometry::Rect;
use tairix_parallel::{JobRunner, Reversed, Threaded, SERIAL};
use tairix_raster::{Color, Surface};

use super::{splat, tent, Bins, Dot, Light, Placed, Spot, MAX_RADIUS, STRIP_ROWS, SUB};
use crate::glow::Glow;
use crate::tone::Tone;

/// A small generator, so a test's spots are the same every run.
struct Spots(u64);

impl Spots {
    fn next(&mut self, below: u32) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        u32::try_from(self.0 % u64::from(below)).expect("below")
    }

    /// `count` spots scattered over `area`, in 256ths of a pixel.
    fn scattered(&mut self, count: usize, area: Rect) -> Vec<Spot> {
        (0..count)
            .map(|_| Spot {
                x: area.left() * SUB + i32::try_from(self.next(area.width * 256)).expect("small"),
                y: area.top() * SUB + i32::try_from(self.next(area.height * 256)).expect("small"),
                light: [400 + self.next(3000), self.next(2000), self.next(4000)]
                    .map(|l| u16::try_from(l).expect("small")),
            })
            .collect()
    }
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
/// taps, and touches no channel it carries no light in.
#[test]
fn a_splat_sheds_the_dot_s_energy() {
    fn shed<const TAPS: usize>(radius: u32) {
        let mut light = alloc::vec![[0u16; 3]; 64 * 64];
        let spot = Spot {
            x: 30 * SUB + 77,
            y: 31 * SUB + 200,
            light: [1000, 0, 20],
        };
        splat::<TAPS>(&spot, (0, 0), 64, &mut light);
        let shed: u64 = light.iter().map(|cell| u64::from(cell[0])).sum();
        let expected = 1000 * Dot::new(radius).energy();
        let taps = u64::from(4 * radius * radius);
        assert!(
            shed.abs_diff(expected) <= taps,
            "radius {radius}: {shed} against {expected}"
        );
        assert!(
            light.iter().all(|cell| cell[1] == 0),
            "no light where none was carried"
        );
    }
    shed::<2>(1);
    shed::<4>(2);
    shed::<6>(3);
    shed::<8>(4);
}

/// A dot drawn where its rows or columns would pass the light it is drawn
/// into is not drawn at all, rather than wrapping onto another row.
#[test]
fn a_splat_past_its_light_is_not_drawn() {
    let mut light = alloc::vec![[0u16; 3]; 16 * 16];
    for (x, y) in [
        (15 * SUB + 200, 8 * SUB),
        (-SUB, 8 * SUB),
        (8 * SUB, -SUB),
        (8 * SUB, 40 * SUB),
    ] {
        splat::<2>(
            &Spot {
                x,
                y,
                light: [500; 3],
            },
            (0, 0),
            16,
            &mut light,
        );
    }
    assert!(light.iter().all(|cell| *cell == [0; 3]));
}

/// Every spot is listed in each strip its dot reaches and in no other.
#[test]
fn a_strip_holds_exactly_the_spots_that_reach_it() {
    let painted = Rect::new(37, 21, 300, 211);
    let dot = Dot::new(2);
    let mut spots = Spots(7).scattered(5_000, Rect::new(30, 10, 320, 240));
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
                let first = dot.first(spot.y);
                let reach = first..first + dot.taps();
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

/// The light of a `side`-pixel frame of one-pixel dots, with its glow if it
/// glows, holding `spots` sorted by `jobs` jobs over the `painted` rows.
fn light(
    side: u32,
    glowing: bool,
    (spots, jobs, painted): (&[Spot], usize, Rect),
    runner: &dyn JobRunner,
) -> Light {
    let glow =
        glowing.then(|| Glow::new((side, side), 4, Dot::new(1).energy(), runner).expect("a glow"));
    let tone = Tone::new(1.0).expect("a tone");
    let per_job = spots.len().div_ceil(jobs);
    let bins = Bins::new(jobs, per_job, 2_000, Dot::new(1)).expect("bins");
    let mut light = Light::new((side, side), bins, (tone, glow), runner).expect("a light");
    for (mut share, traced) in light.shares(painted).zip(spots.chunks(per_job)) {
        share.sort(traced);
    }
    light
}

const MARK: Color = Color::rgb(1, 2, 3);

/// A paint writes every pixel of the painted part of its square opaque, and
/// nothing beyond it, the screen's edge clipping the square.
#[test]
fn a_paint_covers_its_square_on_the_screen_and_nothing_else() {
    let mut surface = Surface::new(400, 300).expect("a surface");
    surface.fill(MARK);
    let frame = Rect::new(-30, 40, 280, 280);
    let painted = frame.intersection(&Rect::new(0, 0, 400, 300));
    let spots = Spots(3).scattered(4_000, Rect::new(20, 80, 200, 200));
    let mut light = light(280, true, (&spots, 5, painted), &SERIAL);
    light.paint(&spots, &Placed { frame, painted }, &mut surface, &SERIAL);
    for y in 0..300 {
        for x in 0..400 {
            let pixel = surface.get(x, y).expect("inside");
            let inside = painted.contains(tairix_geometry::Point::new(
                i32::try_from(x).expect("small"),
                i32::try_from(y).expect("small"),
            ));
            if inside {
                assert_eq!(pixel.a, u8::MAX, "({x}, {y}) opaque");
            } else {
                assert_eq!(
                    (pixel.r, pixel.g, pixel.b),
                    (1, 2, 3),
                    "({x}, {y}) untouched"
                );
            }
        }
    }
    let lit = (0..300)
        .flat_map(|y| (0..400).map(move |x| (x, y)))
        .filter(|&(x, y)| {
            let pixel = surface.get(x, y).expect("inside");
            pixel.r > 1 || pixel.g > 2 || pixel.b > 3
        });
    assert!(lit.count() > 2_000, "the spots are drawn");
}

/// A paint split into bands of rows across cores, whichever band runs first
/// and when they run at once, is the paint drawn on one, pixel for pixel.
#[test]
fn a_paint_in_bands_is_the_paint_drawn_whole() {
    let reversed = Reversed::new(4);
    let threaded = Threaded::new(4);
    let frame = Rect::new(10, 5, 330, 330);
    let screen = Rect::new(0, 0, 360, 320);
    let placed = Placed {
        frame,
        painted: frame.intersection(&screen),
    };
    let spots = Spots(11).scattered(20_000, Rect::new(40, 30, 270, 270));
    let sorted = (spots.as_slice(), 9, placed.painted);
    for glowing in [true, false] {
        let mut whole = Surface::new(360, 320).expect("a surface");
        light(330, glowing, sorted, &SERIAL).paint(&spots, &placed, &mut whole, &SERIAL);
        let runners: [&dyn JobRunner; 2] = [&reversed, &threaded];
        for runner in runners {
            let mut banded = Surface::new(360, 320).expect("a surface");
            light(330, glowing, sorted, runner).paint(&spots, &placed, &mut banded, runner);
            assert!(
                banded == whole,
                "glowing {glowing}: the bands drew the paint"
            );
        }
    }
    assert!(reversed.widest() > 1, "the paint was split");
}
