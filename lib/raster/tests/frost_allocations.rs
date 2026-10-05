//! A frost run from a reserved scratch allocates nothing.
//!
//! The compositor reserves one `BlurScratch` and frost plane with its back
//! buffer and frosts every frame from them, so anything a frost asks the heap
//! for — the list a parallel dispatch hands its pieces out in, a strip grown to
//! the frost's height — is an allocation per frame, and one a machine short of
//! memory can refuse. A counting global allocator holds every shape the
//! compositor frosts to none, on the calling thread and spread across a runner.

use core::ops::Range;

use tairix_fuzzseed::meter::{metered, Metered};
use tairix_parallel::{JobRunner, Reversed, SERIAL};
use tairix_raster::{BlurScratch, Frosting, Pixel, Surface};

#[global_allocator]
static ALLOCATOR: Metered = Metered;

/// How many allocations `run` makes on this thread.
fn allocations(run: impl FnOnce()) -> usize {
    metered(run).1.allocations
}

const SCREEN: (u32, u32) = (240, 200);

/// The rectangle every frost below is a function of.
const RECT: (u32, u32, u32, u32) = (8, 6, 200, 180);

/// A surface whose every pixel differs from its neighbours, so a frost has
/// real averages to take.
fn patterned() -> Surface {
    let mut surface = Surface::new(SCREEN.0, SCREEN.1).expect("surface");
    for y in 0..SCREEN.1 {
        for x in 0..SCREEN.0 {
            let level = |seed: u32| u8::try_from((x * seed + y * 7) % 256).unwrap_or(0);
            surface.set(
                x,
                y,
                Pixel {
                    r: level(3) / 2,
                    g: level(5) / 2,
                    b: level(11) / 2,
                    a: 200,
                },
            );
        }
    }
    surface
}

/// The shapes the compositor frosts: a whole window, the border around a kept
/// core, and two damaged strips far apart.
fn shapes() -> [Vec<(Range<u32>, Range<u32>)>; 3] {
    let (x, y, w, h) = RECT;
    [
        vec![(x..x + w, y..y + h)],
        vec![
            (x..x + w, y..y + 20),
            (x..x + 20, y + 20..y + h - 20),
            (x + w - 20..x + w, y + 20..y + h - 20),
            (x..x + w, y + h - 20..y + h),
        ],
        vec![(x..x + w, y..y + 12), (x..x + w, y + h - 12..y + h)],
    ]
}

#[test]
fn a_frost_from_a_reserved_scratch_allocates_nothing() {
    let shapes = shapes();
    let runners: [&dyn JobRunner; 2] = [&SERIAL, &Reversed::new(4)];
    for runner in runners {
        let mut dest = patterned();
        let mut plane = dest.clone();
        let mut scratch = BlurScratch::new();
        assert!(scratch.reserve(SCREEN.0, SCREEN.1, 8, runner));
        let frost = |dest: &mut Surface, plane: &mut Surface, scratch: &mut BlurScratch| {
            for bands in &shapes {
                let frosting = Frosting {
                    rect: RECT,
                    held: (0..0, 0..0),
                    bands,
                    radius: 9,
                };
                assert!(dest.frost_from(plane, &frosting, scratch, runner, |_, _| 230));
            }
        };
        let allocated = allocations(|| frost(&mut dest, &mut plane, &mut scratch));
        assert_eq!(
            allocated,
            0,
            "a frost from a reserved scratch allocated {allocated} times on a runner {} wide",
            runner.width()
        );
    }
}

#[test]
fn an_in_place_frost_allocates_nothing_once_its_scratch_is_grown() {
    let runners: [&dyn JobRunner; 2] = [&SERIAL, &Reversed::new(4)];
    for runner in runners {
        let mut surface = patterned();
        let mut scratch = BlurScratch::new();
        let (x, y, w, h) = RECT;
        let mut frost = |surface: &mut Surface| {
            surface.frost_region(x, y, w, h, 9, &mut scratch, runner, |_, _| 230);
        };
        frost(&mut surface);
        let allocated = allocations(|| frost(&mut surface));
        assert_eq!(
            allocated,
            0,
            "a second in-place frost allocated {allocated} times on a runner {} wide",
            runner.width()
        );
    }
}
