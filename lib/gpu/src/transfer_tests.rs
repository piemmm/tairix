use tairix_fuzzseed::Prng;

use super::{overlap, spans_meet, spans_overlap, Region};
use crate::format::Aspects;
use crate::image::{Extent3d, Origin3d};

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
    for _ in 0..20_000 {
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
fn the_sweep_finds_every_overlap_the_pairs_of_spans_hold() {
    let mut rng = Prng::new(0x5e_ed5a_a115);
    for _ in 0..20_000 {
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
                .any(|&(b, b_len)| spans_meet(a, a_len, b, b_len))
        });
        assert_eq!(spans_overlap(&mut spans), pairwise, "in {spans:?}");
    }
}
