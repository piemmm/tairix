use tairix_fuzzseed::Prng;

use super::{overlap, sharing, spans_overlap, Axis, Region};
use crate::format::Aspects;
use crate::image::{Extent3d, Origin3d};
use crate::memory::overlaps;

/// The random sets each sweep is held to the pairwise answer on: a sample,
/// so the interpreter, at near thirty milliseconds a set, takes fewer.
const SWEPT_SETS: u32 = if cfg!(miri) { 500 } else { 20_000 };

fn region(rng: &mut Prng) -> Region {
    let small = |rng: &mut Prng, below: u32| rng.next_u32() % below;
    let aspect = [
        Aspects::COLOR,
        Aspects::DEPTH,
        Aspects::STENCIL,
        Aspects::DEPTH_STENCIL,
    ]
    .get(small(rng, 4) as usize)
    .copied()
    .unwrap_or(Aspects::COLOR);
    Region {
        level: small(rng, 2),
        layers: (small(rng, 3), 1 + small(rng, 2)),
        origin: Origin3d {
            x: small(rng, 12),
            y: small(rng, 6),
            z: small(rng, 3),
        },
        extent: Extent3d {
            width: 1 + small(rng, 5),
            height: 1 + small(rng, 3),
            depth: 1 + small(rng, 2),
        },
        aspect,
    }
}

#[test]
fn the_sweep_finds_every_overlap_the_pairs_of_regions_hold() {
    let mut rng = Prng::new(0x5e_ed0f_ce11);
    for _ in 0..SWEPT_SETS {
        let count = 1 + (rng.next_u32() % 10) as usize;
        let mut regions: alloc::vec::Vec<(Region, bool)> = (0..count)
            .map(|_| (region(&mut rng), !rng.next_u32().is_multiple_of(3)))
            .collect();
        let pairwise = regions.iter().enumerate().any(|(at, (a, a_written))| {
            regions
                .iter()
                .skip(at + 1)
                .any(|(b, b_written)| (*a_written || *b_written) && a.meets(b))
        });
        assert_eq!(overlap(&mut regions), pairwise, "in {regions:?}");
    }
}

#[test]
fn rows_of_one_copy_are_swept_along_the_axis_they_are_split_on() {
    let row = |y: u32| {
        let region = Region {
            level: 0,
            layers: (0, 1),
            origin: Origin3d { x: 0, y, z: 0 },
            extent: Extent3d {
                width: 64,
                height: 1,
                depth: 1,
            },
            aspect: Aspects::COLOR,
        };
        (region, true)
    };
    let rows = u32::try_from(crate::MAX_COPY_REGIONS).expect("small");
    let mut regions: alloc::vec::Vec<_> = (0..rows).map(row).collect();
    let n = u64::from(rows);
    assert_eq!(sharing(&mut regions, Axis::X), n * (n - 1) / 2);
    assert_eq!(sharing(&mut regions, Axis::Y), 0, "no pair to test");
    assert!(!overlap(&mut regions));
    regions.push(row(7));
    assert!(overlap(&mut regions), "a row written twice");
}

#[test]
fn the_sweep_finds_every_overlap_the_pairs_of_spans_hold() {
    let mut rng = Prng::new(0x5e_ed5a_a115);
    for _ in 0..SWEPT_SETS {
        let count = 1 + (rng.next_u32() % 10) as usize;
        let mut spans: alloc::vec::Vec<(u64, u64)> = (0..count)
            .map(|_| {
                (
                    u64::from(rng.next_u32() % 64),
                    u64::from(1 + rng.next_u32() % 12),
                )
            })
            .collect();
        let pairwise = spans.iter().enumerate().any(|(at, &(a, a_len))| {
            spans
                .iter()
                .skip(at + 1)
                .any(|&(b, b_len)| overlaps(&(a..a + a_len), &(b..b + b_len)))
        });
        assert_eq!(spans_overlap(&mut spans), pairwise, "in {spans:?}");
    }
}
