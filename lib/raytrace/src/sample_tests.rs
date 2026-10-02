//! Host tests of where samples fall and the order pixels are revealed in.

use alloc::vec;
use alloc::vec::Vec;

use tairix_util::mathf;

use super::{
    cone, cosine_hemisphere, disc, filter_offset, inverse_erf, mix32, unit, Reveal, Sampler,
    Scatter, Step, FILTER_DEVIATION, FILTER_SHARE, FIRST_PASS_ACROSS, SOBOL_PAIRS,
};

/// Pictures that reach every edge of the grid arithmetic: a single pixel, row
/// and column, primes, one too small for a second pass, and sides that are and
/// are not multiples of the first pass's spacing.
const SIZES: [(u32, u32); 10] = [
    (1, 1),
    (1, 7),
    (9, 1),
    (3, 5),
    (16, 9),
    (48, 27),
    (97, 53),
    (100, 37),
    (256, 144),
    (257, 145),
];

const KEYS: [u64; 3] = [0, 42, u64::MAX];

/// Every step of `order`, in order.
fn steps(order: &Reveal) -> Vec<Step> {
    (0..order.count())
        .map(|index| order.step(index).expect("a step within the count"))
        .collect()
}

fn index(step: Step, width: u32) -> usize {
    (step.y * width + step.x) as usize
}

/// Which of `cells × cells` strata of the unit square a point is in.
fn stratum((u, v): (f64, f64), cells: u32) -> usize {
    let side = f64::from(cells);
    let (column, row) = (mathf::floor(u * side), mathf::floor(v * side));
    usize::try_from(mathf::round_i32(row * side + column)).expect("in the square")
}

#[test]
fn every_draw_is_in_the_unit_interval() {
    for seed in [0, 1, 0xdead_beef, u32::MAX] {
        for index in 0..64 {
            let mut sampler = Sampler::new(seed, index);
            for _ in 0..(SOBOL_PAIRS + 8) {
                let (u, v) = sampler.next_2d();
                assert!((0.0..1.0).contains(&u) && (0.0..1.0).contains(&v));
            }
        }
    }
    assert!((unit(u32::MAX) - 1.0).abs() < 1e-9 && unit(u32::MAX) < 1.0);
}

#[test]
fn a_pixel_draws_the_same_numbers_every_time() {
    let draw = |seed, index| {
        let mut sampler = Sampler::new(seed, index);
        [sampler.next_2d(), sampler.next_2d(), sampler.next_2d()]
    };
    assert_eq!(draw(77, 5), draw(77, 5));
    assert_ne!(draw(77, 5), draw(78, 5));
    assert_ne!(draw(77, 5), draw(77, 6));
}

/// The first 4, 16 and 64 samples of any pair fill the 2×2, 4×4 and 8×8
/// strata of the square once each: the Sobol net survives the scrambling and
/// the shuffle, for every pixel.
#[test]
fn each_power_of_four_of_samples_is_stratified_in_every_pair() {
    for seed in [3u32, 0x51ab_c0de, 0xffff_0001] {
        for pair in 0..SOBOL_PAIRS {
            for (count, cells) in [(4u32, 2u32), (16, 4), (64, 8)] {
                let mut filled = vec![0u32; (cells * cells) as usize];
                for index in 0..count {
                    let mut sampler = Sampler::new(seed, index);
                    let mut point = (0.0, 0.0);
                    for _ in 0..=pair {
                        point = sampler.next_2d();
                    }
                    filled[stratum(point, cells)] += 1;
                }
                assert!(
                    filled.iter().all(|hits| *hits == 1),
                    "seed {seed} pair {pair}: {count} samples fill {filled:?}"
                );
            }
        }
    }
}

#[test]
fn two_pairs_of_one_sample_are_not_the_same_numbers() {
    let mut matched = 0;
    for index in 0..256 {
        let mut sampler = Sampler::new(9, index);
        let first = sampler.next_2d();
        let second = sampler.next_2d();
        if (first.0 - second.0).abs() < 1e-3 {
            matched += 1;
        }
    }
    assert!(
        matched < 8,
        "{matched} of 256 samples drew one number twice"
    );
}

#[test]
fn the_disc_map_stays_in_the_disc_and_covers_it_evenly() {
    let mut radius_squared = 0.0;
    let side = 32u32;
    for i in 0..side {
        for j in 0..side {
            let pair = (
                (f64::from(i) + 0.5) / f64::from(side),
                (f64::from(j) + 0.5) / f64::from(side),
            );
            let (x, y) = disc(pair);
            assert!(x * x + y * y <= 1.0 + 1e-12);
            radius_squared += x * x + y * y;
        }
    }
    // Uniform over area: the mean squared radius of a disc is a half.
    let mean = radius_squared / f64::from(side * side);
    assert!((mean - 0.5).abs() < 0.01, "{mean}");
    assert_eq!(disc((0.5, 0.5)), (0.0, 0.0));
}

#[test]
fn a_cone_draw_stays_within_its_angle_and_is_even_in_solid_angle() {
    for cos_max in [0.0, 0.5, 0.99, 0.999_99] {
        let mut mean_z = 0.0;
        let side = 24u32;
        for i in 0..side {
            for j in 0..side {
                let pair = (
                    (f64::from(i) + 0.5) / f64::from(side),
                    (f64::from(j) + 0.5) / f64::from(side),
                );
                let (x, y, z) = cone(cos_max, pair);
                assert!(z >= cos_max - 1e-12, "{z} below {cos_max}");
                assert!((x * x + y * y + z * z - 1.0).abs() < 1e-9);
                mean_z += z;
            }
        }
        let mean = mean_z / f64::from(side * side);
        assert!(
            (mean - 1.0f64.midpoint(cos_max)).abs() < 1e-3,
            "{cos_max}: {mean}"
        );
    }
}

#[test]
fn a_cosine_draw_is_on_the_upper_hemisphere_and_leans_as_the_cosine() {
    let mut mean_z = 0.0;
    let side = 40u32;
    for i in 0..side {
        for j in 0..side {
            let pair = (
                (f64::from(i) + 0.5) / f64::from(side),
                (f64::from(j) + 0.5) / f64::from(side),
            );
            let (x, y, z) = cosine_hemisphere(pair);
            assert!(z >= 0.0);
            assert!((x * x + y * y + z * z - 1.0).abs() < 1e-9);
            mean_z += z;
        }
    }
    // The mean cosine under a cosine-weighted hemisphere is two thirds.
    let mean = mean_z / f64::from(side * side);
    assert!((mean - 2.0 / 3.0).abs() < 0.01, "{mean}");
}

/// The share of a Gaussian of unit deviation within `reach` of its centre,
/// by Simpson's rule.
fn gaussian_share(reach: f64) -> f64 {
    let steps = 20_000u32;
    let h = 2.0 * reach / f64::from(steps);
    let density = |x: f64| mathf::exp(-0.5 * x * x) / mathf::sqrt(2.0 * core::f64::consts::PI);
    let mut total = density(-reach) + density(reach);
    for i in 1..steps {
        let x = -reach + h * f64::from(i);
        total += density(x) * if i % 2 == 1 { 4.0 } else { 2.0 };
    }
    total * h / 3.0
}

#[test]
fn the_filter_keeps_three_deviations_of_a_gaussian() {
    assert!((FILTER_SHARE - gaussian_share(3.0)).abs() < 1e-9);
}

/// The inverse error function undoes the error function it inverts, from the
/// middle out into either tail the filter reaches.
#[test]
fn the_inverse_error_function_inverts_the_gaussian_share() {
    for reach in [0.0, 0.05, 0.4, 1.0, 1.7, 2.5, 3.0] {
        let share = gaussian_share(reach);
        let back = inverse_erf(share) * core::f64::consts::SQRT_2;
        assert!((back - reach).abs() < 1e-5, "{reach}: {back}");
        assert!((inverse_erf(-share) + inverse_erf(share)).abs() < 1e-12);
    }
}

/// The filter's offsets stay within its reach, rise with their draw, mirror
/// about the centre, and fall in each band as often as the truncated
/// Gaussian has them: no sample needs a weight of its own.
#[test]
fn the_filter_draws_its_offsets_as_its_truncated_gaussian() {
    assert!(filter_offset(0.5).abs() < 1e-12);
    let draws = 4096u32;
    let reach = 3.0 * FILTER_DEVIATION;
    let mut last = -reach;
    let mut within_one = 0u32;
    let mut squares = 0.0;
    for i in 0..draws {
        let u = (f64::from(i) + 0.5) / f64::from(draws);
        let offset = filter_offset(u);
        assert!(offset.abs() < reach, "{u}: {offset}");
        assert!(offset > last, "monotone in its draw");
        assert!((offset + filter_offset(1.0 - u)).abs() < 1e-9);
        last = offset;
        within_one += u32::from(offset.abs() <= FILTER_DEVIATION);
        squares += offset * offset;
    }
    let share = f64::from(within_one) / f64::from(draws);
    let expected = gaussian_share(1.0) / FILTER_SHARE;
    assert!(
        (share - expected).abs() < 2e-3,
        "{share} within a deviation"
    );
    // A Gaussian cut off at three deviations keeps 97.3% of its variance.
    let variance = squares / f64::from(draws) / (FILTER_DEVIATION * FILTER_DEVIATION);
    assert!((variance - 0.973).abs() < 0.01, "{variance}");
}

#[test]
fn a_scatter_permutes_its_range_and_leaves_what_is_past_it() {
    for count in [1u32, 2, 3, 7, 64, 65, 97 * 53, 1 << 12, (1 << 12) + 1] {
        for key in KEYS {
            let scatter = Scatter::new(count, key);
            let mut seen = vec![false; count as usize];
            for index in 0..count {
                let to = scatter.permute(index);
                assert!(to < count && !seen[to as usize], "{count}: {index} -> {to}");
                seen[to as usize] = true;
            }
            assert_eq!(scatter.permute(count), count);
            assert_eq!(scatter.permute(u32::MAX), u32::MAX);
        }
    }
}

#[test]
fn every_pixel_is_traced_exactly_once() {
    for size in SIZES {
        for key in KEYS {
            let order = Reveal::new(size, key).expect("a picture");
            assert_eq!(order.count(), size.0 * size.1);
            let mut seen = vec![false; order.count() as usize];
            for step in steps(&order) {
                assert!(step.x < size.0 && step.y < size.1, "{size:?}: {step:?}");
                assert!(!seen[index(step, size.0)], "{size:?}: {step:?} twice");
                seen[index(step, size.0)] = true;
            }
            assert_eq!(order.step(order.count()), None);
            assert_eq!(order.step(u32::MAX), None);
        }
    }
}

#[test]
fn a_picture_with_no_pixels_or_more_than_a_count_holds_has_no_reveal() {
    assert!(Reveal::new((0, 5), 1).is_none());
    assert!(Reveal::new((5, 0), 1).is_none());
    assert!(Reveal::new((1 << 16, 1 << 16), 1).is_none());
    assert!(Reveal::new((u32::MAX, 1), 1).is_some());
}

/// The first pass traces every point of its grid, a hundred-odd pixels
/// however large the screen, so all of the picture lies within a spacing of a
/// traced point once that pass is done.
#[test]
fn the_first_pass_traces_its_whole_grid_in_a_few_points() {
    for size in SIZES
        .into_iter()
        .chain([(1920, 1080), (3840, 2160), (1080, 1920)])
    {
        let (width, height) = size;
        let side = Reveal::coarsest(size);
        assert!(side.is_power_of_two());
        let shorter = width.min(height);
        if shorter >= FIRST_PASS_ACROSS {
            assert!(side * FIRST_PASS_ACROSS <= shorter && shorter < 2 * side * FIRST_PASS_ACROSS);
        } else {
            assert_eq!(side, 1);
        }
        let points = width.div_ceil(side) * height.div_ceil(side);
        let order = Reveal::new(size, 3).expect("a picture");
        let mut first: Vec<(u32, u32)> = (0..points)
            .map(|at| order.step(at).expect("a first-pass step"))
            .inspect(|step| assert_eq!(step.side, side, "{size:?}: {step:?}"))
            .map(|step| (step.x, step.y))
            .collect();
        first.sort_unstable();
        let mut grid: Vec<(u32, u32)> = (0..width.div_ceil(side))
            .flat_map(|column| (0..height.div_ceil(side)).map(move |row| (column, row)))
            .map(|(column, row)| (column * side, row * side))
            .collect();
        grid.sort_unstable();
        assert_eq!(first, grid, "{size:?}");
    }
    assert_eq!(Reveal::coarsest((1920, 1080)), 128);
}

/// The passes run coarsest first, each halving the last one's spacing, each
/// step a point of its own pass's grid that the grid of twice its spacing
/// does not hold; and once a pass ends, every point of its grid is traced.
#[test]
fn every_pass_halves_the_grid_and_ends_with_it_whole() {
    for size in SIZES {
        for key in KEYS {
            let order = Reveal::new(size, key).expect("a picture");
            let (width, height) = size;
            let mut traced = vec![false; (width * height) as usize];
            let mut side = Reveal::coarsest(size);
            let whole = |traced: &[bool], side: u32| {
                (0..height).step_by(side as usize).all(|y| {
                    (0..width)
                        .step_by(side as usize)
                        .all(|x| traced[(y * width + x) as usize])
                })
            };
            for step in steps(&order) {
                if step.side != side {
                    assert_eq!(step.side * 2, side, "{size:?}: a pass is half the last");
                    assert!(whole(&traced, side), "{size:?}: pass {side} left a point");
                    side = step.side;
                }
                assert_eq!((step.x % side, step.y % side), (0, 0), "{step:?}");
                if side < Reveal::coarsest(size) {
                    assert_ne!(
                        (step.x % (2 * side), step.y % (2 * side)),
                        (0, 0),
                        "{size:?}: {step:?} is the coarser grid's"
                    );
                }
                traced[index(step, width)] = true;
            }
            assert_eq!(side, 1, "{size:?}: the last pass traces single pixels");
            assert!(traced.iter().all(|pixel| *pixel));
        }
    }
}

/// Within a pass the steps are spread over the whole picture: the first tenth
/// of the last pass reaches every part of the screen in about equal share, and
/// successive steps land far apart, where an order running down the screen
/// would sharpen one band alone.
#[test]
fn a_pass_sharpens_the_whole_picture_at_once() {
    let (width, height) = (256u32, 144u32);
    let (tiles_x, tiles_y) = (8u32, 8u32);
    for key in [1u64, 7, 0x5eed] {
        let order = Reveal::new((width, height), key).expect("a picture");
        let last_pass = width * height - width.div_ceil(2) * height.div_ceil(2);
        let first = order.count() - last_pass;
        let tenth = last_pass / 10;
        let mut tiles = vec![0u32; (tiles_x * tiles_y) as usize];
        let mut travel = 0u64;
        let mut last = order.step(first).expect("a step");
        for at in first..first + tenth {
            let step = order.step(at).expect("a step");
            assert_eq!(step.side, 1);
            tiles[(step.y * tiles_y / height * tiles_x + step.x * tiles_x / width) as usize] += 1;
            travel += u64::from(step.x.abs_diff(last.x) + step.y.abs_diff(last.y));
            last = step;
        }
        let share = tenth / (tiles_x * tiles_y);
        for (tile, hits) in tiles.iter().enumerate() {
            assert!(
                *hits > share / 2 && *hits < share * 2,
                "key {key}: tile {tile} has {hits}, its share {share}"
            );
        }
        let mean_step = travel / u64::from(tenth);
        assert!(mean_step > u64::from(width + height) / 4, "{mean_step}");
    }
}

/// The key orders the steps within each pass and decides nothing else: the
/// same key gives the same reveal, and another key the same passes in another
/// order.
#[test]
fn a_key_orders_each_pass_and_nothing_else() {
    let picture = (97, 53);
    let one = steps(&Reveal::new(picture, 1).expect("a picture"));
    assert_eq!(one, steps(&Reveal::new(picture, 1).expect("a picture")));
    let two = steps(&Reveal::new(picture, 2).expect("a picture"));
    assert_ne!(one, two);
    for (a, b) in one.iter().zip(&two) {
        assert_eq!(a.side, b.side);
    }
}

#[test]
fn the_hash_finaliser_spreads_neighbouring_inputs() {
    let flips: u32 = (0..1024u32)
        .map(|x| (mix32(x) ^ mix32(x + 1)).count_ones())
        .sum();
    let mean = f64::from(flips) / 1024.0;
    assert!((mean - 16.0).abs() < 1.5, "{mean}");
}
