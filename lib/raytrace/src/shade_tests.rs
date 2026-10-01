use super::*;

const SQUARE: Rect = ((-60.0, -60.0), (60.0, 60.0));

/// Whether a shade's reading `at` is open sky.
fn open_at(at: (f64, f64)) -> bool {
    at.0.abs() < 1e-12 && at.1.abs() < 1e-12
}

#[test]
fn a_lone_crown_shades_beneath_it_but_hides_little_of_the_sky() {
    let canopy = Shade::of(&[((0.0, 0.0), 6.0)], SQUARE, 0.5, 12.0).expect("held");
    assert!(!canopy.is_open());
    let (under, hidden) = canopy.at(0.0, 0.0);
    assert!(under > 0.99, "at the trunk: {under}");
    assert!(
        (0.08..0.35).contains(&hidden),
        "a lone tree roofs little of the sky: {hidden}"
    );
    assert!(canopy.at(0.5 * 6.0, 0.0).0 > 0.95);
    assert!(canopy.at(6.1, 0.0).0 < 0.01, "past the crown's reach");
    assert!(open_at(canopy.at(40.0, 40.0)), "far off, open sky");
    assert!(open_at(canopy.at(400.0, 0.0)), "off the grid");
}

#[test]
fn a_closed_wood_hides_nearly_all_the_sky() {
    // Trunks six metres apart in rows staggered by half that, crowns five
    // metres across: as close as a wood's canopy closes.
    let (apart, row_apart) = (6.0, 6.0 * mathf::sqrt(0.75));
    let mut crowns = Vec::new();
    for row in -14..=14 {
        let offset = if row % 2 == 0 { 0.0 } else { 0.5 * apart };
        for column in -12..=12 {
            crowns.push((
                (
                    f64::from(column) * apart + offset,
                    f64::from(row) * row_apart,
                ),
                5.0,
            ));
        }
    }
    let canopy = Shade::of(&crowns, SQUARE, 0.5, 12.0).expect("held");
    let gap = (0.5 * apart, row_apart / 3.0);
    for probe in [(0.0, 0.0), gap, (10.0, -17.0)] {
        let (under, hidden) = canopy.at(probe.0, probe.1);
        assert!(under > 0.7, "{probe:?}: {under}");
        assert!(hidden > 0.85, "{probe:?}: {hidden}");
    }
}

#[test]
fn no_crown_over_the_rectangle_is_open_ground() {
    for crowns in [&[((200.0, 0.0), 8.0)][..], &[]] {
        let shade = Shade::of(crowns, SQUARE, 1.0, 10.0).expect("held");
        assert!(shade.is_open());
        assert!(open_at(shade.at(0.0, 0.0)));
    }
}

#[test]
fn a_lands_shade_is_fine_about_the_eye_coarse_beyond_and_crops_as_it_reads() {
    let crowns = [
        ((5.0, -3.0), 7.0),
        ((-20.0, 12.0), 4.0),
        ((400.0, 300.0), 9.0),
        ((-600.0, 0.0), 6.0),
    ];
    let shades = Shades::of(&crowns, ((0.0, 0.0), 1000.0), (0.0, 0.0)).expect("held");
    // Near the eye, a crown's edge is sharp; far off, it is still there.
    assert!(shades.at(5.0, -3.0).0 > 0.99 && shades.at(12.5, -3.0).0 < 0.01);
    assert!(
        shades.at(400.0, 300.0).0 > 0.5,
        "a far crown shades the coarse grid"
    );
    assert!(open_at(shades.at(-300.0, -300.0)));
    let part = shades
        .within(((-30.0, -20.0), (20.0, 25.0)), 1.0)
        .expect("held");
    assert!(!part.is_open());
    for probe in [(0.0, 0.0), (5.0, -3.0), (-19.0, 11.5), (10.0, 20.0)] {
        let (whole, cropped) = (shades.at(probe.0, probe.1), part.at(probe.0, probe.1));
        assert!(
            (whole.0 - cropped.0).abs() < 0.08 && (whole.1 - cropped.1).abs() < 0.08,
            "{probe:?}: {whole:?} {cropped:?}"
        );
    }
    assert!(shades
        .within(((100.0, 100.0), (120.0, 120.0)), 1.0)
        .expect("held")
        .is_open());
    let copy = shades.copied().expect("held");
    assert!(open_at((
        copy.at(7.0, 1.0).0 - shades.at(7.0, 1.0).0,
        copy.at(7.0, 1.0).1 - shades.at(7.0, 1.0).1
    )));
}

#[test]
fn smoothing_keeps_a_level_run_level_and_spreads_a_step() {
    let close = |a: &[f64], b: &[f64]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-12);
    let mut sums = [0.0; 16];
    let mut level = [4.0; 10];
    smooth(&mut level, 1, 3, &mut sums);
    assert!(close(&level, &[4.0; 10]), "{level:?}");
    let mut step = [0.0, 0.0, 0.0, 0.0, 0.0, 9.0, 9.0, 9.0, 9.0, 9.0];
    smooth(&mut step, 1, 1, &mut sums);
    assert!(
        close(&step, &[0.0, 0.0, 0.0, 0.0, 3.0, 6.0, 9.0, 9.0, 9.0, 9.0]),
        "{step:?}"
    );
    // Along a column of a grid two wide, the other column untouched.
    let mut grid = [1.0, 7.0, 1.0, 7.0, 4.0, 7.0];
    smooth(&mut grid, 2, 1, &mut sums);
    assert!(close(&grid, &[1.0, 7.0, 2.0, 7.0, 3.0, 7.0]), "{grid:?}");
}
