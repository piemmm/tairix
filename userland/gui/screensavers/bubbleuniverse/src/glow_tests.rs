//! Host tests of the glow: what a spot leaves, how it spreads and falls off,
//! and how a pixel reads it back.

extern crate std;

use alloc::vec::Vec;

use tairix_parallel::{JobRunner, Reversed, Threaded, SERIAL};

use super::{clamped, lerp, Between, Glow, DARK};

/// A square frame of `side` pixels with cells of `cell`, for dots of one
/// pixel's energy.
fn glow(side: u32, cell: u32) -> Glow {
    Glow::new((side, side), cell, 1, &SERIAL).expect("a glow")
}

/// What `glow` gathered, summed over every cell and channel.
fn gathered(glow: &Glow) -> u64 {
    glow.gathered
        .iter()
        .map(|cell| u64::from(cell.r) + u64::from(cell.g) + u64::from(cell.b))
        .sum()
}

/// The glow along pixel row `row` of a frame `side` pixels wide, at each
/// pixel column, as a band reads it.
fn row(glow: &Glow, row: u32, side: u32) -> Vec<[u32; 3]> {
    let width = usize::try_from(side).expect("small");
    let between = glow.rows_about(row);
    let mut cells = alloc::vec![[0; 3]; glow.across()];
    let (mut upper, mut lower) = (alloc::vec![[0; 3]; width], alloc::vec![[0; 3]; width]);
    glow.row(between.cell(), 0, &mut cells, &mut upper);
    glow.row(between.cell() + 1, 0, &mut cells, &mut lower);
    upper
        .iter()
        .zip(&lower)
        .map(|(upper, lower)| {
            core::array::from_fn(|c| {
                lerp(u32::from(upper[c]), u32::from(lower[c]), between.weight())
            })
        })
        .collect()
}

/// Wherever a spot falls between cells, the four about it take its whole
/// deposit between them.
#[test]
fn a_spot_leaves_its_whole_deposit_wherever_it_falls() {
    let mut glow = glow(240, 6);
    let light = [4000, 1000, 250];
    let whole: u64 = light
        .iter()
        .map(|&l| (u64::from(l) * glow.deposit) >> 16)
        .sum();
    for (x, y) in [
        (120 * 256, 120 * 256),
        (100 * 256 + 37, 133 * 256 + 211),
        (61 * 256 + 128, 90 * 256 + 1),
    ] {
        glow.clear();
        glow.gather((x, y), light);
        let left = gathered(&glow);
        assert!(
            left.abs_diff(whole) <= 6,
            "({x}, {y}): {left} against {whole}"
        );
    }
}

#[test]
fn nothing_gathered_glows_nowhere() {
    let mut glow = glow(240, 6);
    glow.clear();
    glow.spread(&SERIAL);
    assert!(glow.halo.iter().all(|cell| *cell == DARK));
    for pixel_row in [0, 100, 239] {
        assert!(row(&glow, pixel_row, 240)
            .iter()
            .all(|light| *light == [0; 3]));
    }
}

/// One bright spot at a cell's centre haloes that cell's neighbours alike on
/// every side, its glow falls away from where it fell, and it is gone well
/// inside the reach the frame allows the glow.
#[test]
fn a_spot_s_glow_falls_away_from_where_it_fell() {
    let (side, cell) = (480, 6);
    let mut glow = glow(side, cell);
    glow.clear();
    // Cell 40's centre, 243 pixels in.
    let (spot_cell, spot) = (40usize, 243 * 256);
    for _ in 0..64 {
        glow.gather((spot, spot), [u16::MAX; 3]);
    }
    glow.spread(&SERIAL);
    let across = glow.across();
    let halo = |x: usize, y: usize| glow.halo[y * across + x].r;
    for k in 0..8 {
        assert_eq!(
            halo(spot_cell + k, spot_cell),
            halo(spot_cell - k, spot_cell),
            "{k}"
        );
        assert_eq!(
            halo(spot_cell, spot_cell + k),
            halo(spot_cell + k, spot_cell),
            "{k}"
        );
    }
    let middle = row(&glow, 243, side);
    let at = |x: u32| middle[usize::try_from(x).expect("small")][0];
    assert!(at(243) > 0);
    for step in 1..150 {
        assert!(at(242 - step) <= at(242 - step + 1), "left of it: {step}");
    }
    // The haze gathers the spot's cell into a block centred a cell and a half
    // to its right; past that, the glow falls away on that side too.
    for step in 24..150 {
        assert!(at(243 + step) <= at(243 + step - 1), "right of it: {step}");
    }
    let below = row(&glow, 243 + 60, side);
    assert!(below[243][0] < at(243), "and below it");
    let far = row(&glow, 4, side);
    assert!(
        far.iter().all(|light| *light == [0; 3]),
        "far off it is gone"
    );
}

/// The halo spread a band of rows at a time across cores, whichever band runs
/// first and when they run at once, is the halo spread whole, cell for cell.
#[test]
fn a_halo_spread_in_bands_is_the_halo_spread_whole() {
    let spread = |runner: &dyn JobRunner| {
        let mut glow = Glow::new((600, 600), 5, 1, runner).expect("a glow");
        glow.clear();
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        for _ in 0..3_000 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let at = |shift: u32| u32::try_from((seed >> shift) % (600 * 256)).expect("small");
            glow.gather((at(0), at(24)), [900, 300, 1200]);
        }
        glow.spread(runner);
        (glow.halo, glow.haze, glow.blocks.len())
    };
    let (whole, haze, bands) = spread(&SERIAL);
    assert_eq!(bands, 1);
    let reversed = Reversed::new(4);
    let threaded = Threaded::new(4);
    let runners: [&dyn JobRunner; 2] = [&reversed, &threaded];
    for runner in runners {
        let (banded, banded_haze, bands) = spread(runner);
        assert!(bands > 1, "the halo was split");
        assert!(banded == whole, "the bands spread the halo");
        assert!(banded_haze == haze, "and the haze gathered from it");
    }
}

/// A position before the first cell's centre or past the last is held at it,
/// and one between two centres weighs the further by how near it lies.
#[test]
fn a_position_lies_between_the_two_nearest_cell_centres() {
    let cells = 10;
    assert_eq!(clamped(0, cells), Between { cell: 0, weight: 0 });
    assert_eq!(clamped(128, cells), Between { cell: 0, weight: 0 });
    assert_eq!(
        clamped(128 + 64, cells),
        Between {
            cell: 0,
            weight: 64
        }
    );
    assert_eq!(
        clamped(3 * 256 + 128 + 200, cells),
        Between {
            cell: 3,
            weight: 200
        }
    );
    assert_eq!(
        clamped(8 * 256 + 128, cells),
        Between { cell: 8, weight: 0 }
    );
    assert_eq!(
        clamped(9 * 256 + 128, cells),
        Between {
            cell: 8,
            weight: 256
        }
    );
    assert_eq!(
        clamped(u32::MAX, cells),
        Between {
            cell: 8,
            weight: 256
        }
    );
    let cells = [[0, 0, 0], [256, 512, 1024], [0; 3]];
    assert_eq!(
        Between {
            cell: 0,
            weight: 128
        }
        .sample(&cells),
        [128, 256, 512]
    );
    assert_eq!(
        Between { cell: 2, weight: 0 }.sample(&cells),
        [0; 3],
        "past the row"
    );
}
