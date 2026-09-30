//! Host tests of where samples fall and the order pixels are revealed in.

use alloc::vec;

use tairix_util::mathf;

use super::{cone, cosine_hemisphere, disc, mix32, tent, unit, Reveal, Sampler, SOBOL_PAIRS};

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

#[test]
fn the_tent_is_centred_bounded_and_symmetric() {
    assert!(tent(0.5).abs() < 1e-12);
    assert!((tent(0.0) + 1.0).abs() < 1e-12);
    assert!(tent(0.999_999_9) < 1.0);
    let mut total = 0.0;
    for i in 0..1000u32 {
        let u = (f64::from(i) + 0.5) / 1000.0;
        let t = tent(u);
        assert!((-1.0..=1.0).contains(&t));
        assert!((t + tent(1.0 - u)).abs() < 1e-9);
        total += t.abs();
    }
    // A tent's mean distance from its centre is a third of its reach.
    assert!((total / 1000.0 - 1.0 / 3.0).abs() < 0.01);
}

#[test]
fn the_reveal_visits_every_pixel_exactly_once() {
    for count in [
        1u32,
        2,
        3,
        7,
        64,
        65,
        97 * 53,
        1 << 12,
        (1 << 12) + 1,
        160 * 90,
    ] {
        for key in [0u64, 42, u64::MAX] {
            let order = Reveal::new(count, key);
            let mut seen = vec![false; count as usize];
            for index in 0..count {
                let pixel = order.pixel(index);
                assert!(pixel < count, "{count}: {pixel}");
                assert!(!seen[pixel as usize], "{count}: {pixel} twice");
                seen[pixel as usize] = true;
            }
        }
    }
    assert_eq!(Reveal::new(0, 1).pixel(0), 0);
}

/// Past its end the order counts on round from its start, so no index is
/// ever walked forever outside the picture.
#[test]
fn the_order_counts_round_past_its_end() {
    for count in [1u32, 3, 5, 97 * 53, (1 << 12) + 1] {
        for key in [0u64, 42, u64::MAX] {
            let order = Reveal::new(count, key);
            for index in [0, 1, count / 2, count - 1] {
                assert_eq!(order.pixel(index + count), order.pixel(index));
                assert_eq!(order.pixel(index + 3 * count), order.pixel(index));
            }
            assert!(order.pixel(u32::MAX) < count);
        }
    }
}

/// However far it has got, the reveal is spread over the whole picture: the
/// first tenth of it reaches every part of the screen in about equal share,
/// where an order running down the screen would reach one band alone.
#[test]
fn the_reveal_scatters_over_the_whole_picture_from_the_start() {
    let (width, height) = (256u32, 144u32);
    let (tiles_x, tiles_y) = (8u32, 8u32);
    for key in [1u64, 7, 0x5eed] {
        let order = Reveal::new(width * height, key);
        let tenth = width * height / 10;
        let mut tiles = vec![0u32; (tiles_x * tiles_y) as usize];
        let mut travel = 0u64;
        let mut last = order.pixel(0);
        for index in 0..tenth {
            let pixel = order.pixel(index);
            let (x, y) = (pixel % width, pixel / width);
            tiles[(y * tiles_y / height * tiles_x + x * tiles_x / width) as usize] += 1;
            let (lx, ly) = (last % width, last / width);
            travel += u64::from(x.abs_diff(lx) + y.abs_diff(ly));
            last = pixel;
        }
        let share = tenth / (tiles_x * tiles_y);
        for (tile, hits) in tiles.iter().enumerate() {
            assert!(
                *hits > share / 2 && *hits < share * 2,
                "key {key}: tile {tile} has {hits}, its share {share}"
            );
        }
        // Successive pixels land far apart, not beside each other.
        let mean_step = travel / u64::from(tenth);
        assert!(mean_step > u64::from(width + height) / 4, "{mean_step}");
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
