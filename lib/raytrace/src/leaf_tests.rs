//! Host tests of leaf outlines: each covers its own blade, symmetric about
//! the midrib, and leaves the gaps its shape has.

use super::*;

const OUTLINES: [Outline; 9] = [
    Outline::Ovate { teeth: 8 },
    Outline::Lanceolate,
    Outline::Lobed { lobes: 4 },
    Outline::Palmate { lobes: 5 },
    Outline::Shoot { count: 12 },
    Outline::Fascicle { count: 9 },
    Outline::Trefoil,
    Outline::Runcinate,
    Outline::Strap { from: 0, to: 255 },
];

#[test]
fn every_outline_is_symmetric_about_its_midrib_and_stays_in_its_blade() {
    for outline in OUTLINES {
        let mut covered = 0;
        for i in 0..=40u32 {
            for j in 0..=40u32 {
                let (u, v) = (f64::from(i) / 40.0, f64::from(j) / 20.0 - 1.0);
                assert_eq!(
                    outline.covers(u, v),
                    outline.covers(u, -v),
                    "{outline:?} at {u} {v}"
                );
                covered += u32::from(outline.covers(u, v));
            }
        }
        assert!(covered > 40, "{outline:?} covers {covered} of 1681");
        assert!(!outline.covers(-0.01, 0.0) && !outline.covers(1.01, 0.0));
        assert!(!outline.covers(0.5, 1.01), "{outline:?}");
    }
}

#[test]
fn a_leaf_covers_its_midrib_and_its_shape_leaves_its_gaps() {
    for outline in [
        Outline::Ovate { teeth: 0 },
        Outline::Lanceolate,
        Outline::Lobed { lobes: 4 },
        Outline::Runcinate,
    ] {
        assert!(outline.covers(0.5, 0.0), "{outline:?}");
        assert!((outline.off_midrib(0.5, 0.0)).abs() < 1e-12);
    }
    // Between a shoot's needles lies open air.
    let shoot = Outline::Shoot { count: 12 };
    let gaps = (0..200u32)
        .filter(|&step| !shoot.covers(f64::from(step) / 200.0, 0.3))
        .count();
    assert!(gaps > 60, "{gaps}");
    // A lobed leaf's sinuses bite in from its edge.
    let lobed = Outline::Lobed { lobes: 4 };
    let edge = |u: f64| {
        (0..100u32)
            .map(|j| f64::from(j) / 100.0)
            .take_while(|&v| lobed.covers(u, v))
            .count()
    };
    let widths: alloc::vec::Vec<usize> = (10..90u32).map(|i| edge(f64::from(i) / 100.0)).collect();
    let (least, most) = (
        widths.iter().min().copied().unwrap_or(0),
        widths.iter().max().copied().unwrap_or(0),
    );
    assert!(most > least + 15, "lobes and sinuses: {least}..{most}");
}

#[test]
fn a_strap_leaf_in_pieces_keeps_the_one_outline_drawn_to_its_point() {
    let marks = [0u8, 85, 170, 255];
    let pieces: alloc::vec::Vec<Outline> = marks
        .windows(2)
        .map(|ends| Outline::Strap {
            from: ends[0],
            to: ends[1],
        })
        .collect();
    for pair in pieces.windows(2) {
        for step in 0..=100u32 {
            let v = f64::from(step) / 100.0;
            assert_eq!(
                pair[0].covers(1.0, v),
                pair[1].covers(0.0, v),
                "{pair:?} at {v}"
            );
        }
    }
    let whole = Outline::Strap { from: 0, to: 255 };
    assert!(whole.covers(0.5, 0.95), "full width along its middle");
    assert!(!whole.covers(1.0, 0.05), "drawn to a point");
    assert!(!whole.covers(0.0, 0.7), "narrower where it sheathes");
}
