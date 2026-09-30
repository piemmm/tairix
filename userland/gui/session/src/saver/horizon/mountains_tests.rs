//! Host tests of the mountain ranges: a seed's own ranges, the valley they
//! keep clear for the sun, their place above the horizon, and the order they
//! are drawn in.

use alloc::vec::Vec;

use tairix_raster::{Canvas, ScanScratch};
use tairix_util::mathf;
use tairix_wm::{Color, Rect, Scale, Surface};

use super::Mountains;
use crate::saver::horizon::View;

const SCREEN: (u32, u32) = (960, 540);

fn view() -> View {
    View::new(SCREEN, Scale::ONE).expect("a view")
}

fn whole() -> Rect {
    Rect::new(0, 0, SCREEN.0, SCREEN.1)
}

/// The ranges drawn alone, over transparency.
fn drawn(seed: u64) -> Surface {
    let mut surface = Surface::new(SCREEN.0, SCREEN.1).expect("a surface");
    Mountains::new(&view(), seed)
        .expect("ranges")
        .draw(&mut surface, whole());
    surface
}

/// The highest row the ranges reach in column `x`, if they reach it at all.
fn summit(surface: &Surface, x: u32) -> Option<u32> {
    (0..SCREEN.1).find(|y| surface.get(x, *y).is_some_and(|pixel| pixel.a > 128))
}

#[test]
fn a_seed_raises_its_own_ranges_every_time() {
    let (one, again, other) = (drawn(3), drawn(3), drawn(4));
    assert_eq!(one.pixels(), again.pixels());
    assert_ne!(one.pixels(), other.pixels());
}

/// However the peaks fall, the ranges keep the valley the sun sets into
/// clear of its upper half, and rise towards the screen's edges.
#[test]
fn the_ranges_keep_the_valley_clear_and_rise_towards_the_edges() {
    let view = view();
    let (centre, radius) = view.sun;
    for seed in 0..12 {
        let surface = drawn(seed * 1_000_003);
        let middle = u32::try_from(mathf::round_i32(view.centre)).expect("on screen");
        let reach = u32::try_from(mathf::round_i32(radius * 0.4)).expect("small");
        for x in middle - reach..middle + reach {
            if let Some(top) = summit(&surface, x) {
                assert!(
                    f64::from(top) > centre,
                    "seed {seed}: column {x} rises to {top}"
                );
            }
        }
        let tallest = |columns: core::ops::Range<u32>| {
            columns
                .filter_map(|x| summit(&surface, x))
                .min()
                .unwrap_or(SCREEN.1)
        };
        let edges = tallest(0..SCREEN.0 / 6).min(tallest(SCREEN.0 * 5 / 6..SCREEN.0));
        let valley = tallest(middle - reach..middle + reach);
        assert!(
            edges < valley,
            "seed {seed}: the edges rise above the valley"
        );
    }
}

#[test]
fn the_ranges_stand_above_the_horizon_alone() {
    let view = view();
    for seed in [1, 2, 3] {
        let mut surface = Surface::new(SCREEN.0, SCREEN.1).expect("a surface");
        let runner = tairix_parallel::Reversed::new(4);
        Mountains::new(&view, seed)
            .expect("ranges")
            .paint(&mut surface, &runner);
        for y in view.horizon..SCREEN.1 {
            for x in 0..SCREEN.0 {
                let pixel = surface.get(x, y).expect("in bounds");
                assert_eq!(pixel.a, 0, "({x}, {y})");
            }
        }
        assert!(
            (0..view.horizon).any(|y| surface.get(0, y).is_some_and(|pixel| pixel.a > 0)),
            "the ranges reach the screen's edge"
        );
    }
}

/// A canvas that keeps every shape filled onto it, in order.
#[derive(Default)]
struct Recorder {
    fills: Vec<(Vec<(i32, i32)>, Color)>,
}

impl Canvas for Recorder {
    fn fill_polygon_subpixel(&mut self, polygon: &[(i32, i32)], color: Color, _: &mut ScanScratch) {
        self.fills.push((polygon.to_vec(), color));
    }
}

/// Every edge is drawn once — its glow, then its core — and only once both
/// faces beside it are down, so a later face never covers part of an edge
/// that belongs on top of it.
#[test]
fn every_edge_is_drawn_once_after_its_faces() {
    let mountains = Mountains::new(&view(), 9).expect("ranges");
    let mut recorder = Recorder::default();
    mountains.draw(&mut recorder, whole());
    let faces = recorder
        .fills
        .iter()
        .filter(|(shape, _)| shape.len() == 3)
        .count();
    let cores: Vec<(usize, &Vec<(i32, i32)>)> = recorder
        .fills
        .iter()
        .enumerate()
        .filter(|(_, (shape, color))| shape.len() == 4 && color.a == u8::MAX)
        .map(|(at, (shape, _))| (at, shape))
        .collect();
    let glows = recorder
        .fills
        .iter()
        .filter(|(shape, color)| shape.len() == 4 && color.a < u8::MAX)
        .count();
    assert!(faces > 100, "{faces} faces");
    assert_eq!(glows, cores.len(), "each core has its glow");
    // A core is keyed by the segment it strokes: its quad's midpoints.
    let segment = |quad: &Vec<(i32, i32)>| {
        let mid = |a: (i32, i32), b: (i32, i32)| (i32::midpoint(a.0, b.0), i32::midpoint(a.1, b.1));
        let (from, to) = (mid(quad[0], quad[1]), mid(quad[2], quad[3]));
        if from <= to {
            (from, to)
        } else {
            (to, from)
        }
    };
    let mut segments: Vec<((i32, i32), (i32, i32))> =
        cores.iter().map(|(_, quad)| segment(quad)).collect();
    let drawn = segments.len();
    segments.sort_unstable();
    segments.dedup();
    assert_eq!(segments.len(), drawn, "no edge drawn twice");
    // Each cell drawn holds two faces and three edges of its own.
    assert!(2 * drawn >= 3 * faces, "{drawn} edges for {faces} faces");
    // No face beside an edge is filled after it.
    let near = |a: (i32, i32), b: (i32, i32)| (a.0 - b.0).abs() <= 1 && (a.1 - b.1).abs() <= 1;
    for (at, quad) in &cores {
        let (from, to) = segment(quad);
        let beside = recorder.fills.iter().enumerate().filter(|(_, (shape, _))| {
            shape.len() == 3
                && shape.iter().any(|corner| near(*corner, from))
                && shape.iter().any(|corner| near(*corner, to))
        });
        for (face, _) in beside {
            assert!(
                face < *at,
                "a face beside the edge at {at} was filled after it"
            );
        }
    }
}
