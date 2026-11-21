//! Host tests of the glow: what a spot leaves, how it softens, spreads and
//! falls off, how the haze gathers it, and how a pixel reads it back.

extern crate std;

use alloc::vec::Vec;

use tairix_parallel::{JobRunner, Reversed, Threaded, SERIAL};
use tairix_raster::Pixel16;
use tairix_rng::{NonCryptoRng, RandU64};

use super::{
    clamped, gather_haze, lerp, soften, Between, Glow, DEPOSIT_SHIFT, GLOW_CELLS, HAZE_CELLS,
    SOFTEN,
};
use crate::light::Shed;

/// A square frame of `side` pixels with cells of `cell`, for dots of one
/// pixel's energy whose spots hold their light as the reference screen's do.
fn glow(side: u32, cell: u32, runner: &dyn JobRunner) -> Glow {
    Glow::new((side, side), cell, (1, Shed::new(0.5)), runner).expect("a glow")
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
    glow.row(between.cell(), 0..width, &mut cells, &mut upper);
    glow.row(between.cell() + 1, 0..width, &mut cells, &mut lower);
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
    let mut glow = glow(240, 6, &SERIAL);
    let light = [60_000, 15_000, 3_750];
    let point = DEPOSIT_SHIFT + glow.shift;
    let whole: u64 = light
        .iter()
        .map(|&l| (u64::from(l) * glow.deposit) >> point)
        .sum();
    assert!(whole > 300, "{whole}");
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
    let mut glow = glow(240, 6, &SERIAL);
    glow.clear();
    glow.spread(&SERIAL);
    assert!(glow.halo.iter().all(|cell| *cell == Pixel16::BLACK));
    for pixel_row in [0, 100, 239] {
        assert!(row(&glow, pixel_row, 240)
            .iter()
            .all(|light| *light == [0; 3]));
    }
}

/// A cell's light glows as itself while faint and ever less than itself as it
/// brightens, so a crowd glows more than a lone point but never past the
/// softening's ceiling, however many points it holds.
#[test]
fn a_crowd_glows_more_than_a_point_but_never_as_all_of_its_points() {
    let mut last = 0;
    for light in (0..=Pixel16::FULL).step_by(7) {
        let glows = soften(light);
        assert!(glows >= last && glows <= light, "{light}: {glows}");
        assert!(u32::from(glows) < SOFTEN, "{light}: {glows}");
        last = glows;
    }
    assert!(
        soften(10).abs_diff(10) <= 1,
        "a faint light glows as itself"
    );
    assert!(soften(Pixel16::FULL) > soften(1_500), "a crowd glows more");
}

/// One bright spot at a cell's centre haloes that cell's neighbours alike on
/// every side, and its glow falls away from where it fell: to under a
/// hundredth of its peak at the reach the frame allows the glow, and to
/// nothing past it.
#[test]
fn a_spot_s_glow_falls_away_from_where_it_fell() {
    let (side, cell) = (480, 6);
    let mut glow = glow(side, cell, &SERIAL);
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
    let reach = u32::try_from(GLOW_CELLS).expect("small") * cell;
    let edge = row(&glow, 243 - reach, side);
    assert!(
        100 * edge[243][0] <= at(243) && 100 * at(243 - reach) <= at(243),
        "at the reach: {} and {} against {}",
        edge[243][0],
        at(243 - reach),
        at(243)
    );
    let far = row(&glow, 4, side);
    assert!(far.iter().all(|light| *light == [0; 3]), "past it, nothing");
}

/// A row read out at some of the frame's columns is those columns of the row
/// read out whole, and nothing past them is written; columns past the frame
/// read out dark.
#[test]
fn a_row_read_at_some_columns_is_those_columns_of_the_whole_row() {
    const MARK: [u16; 3] = [7, 7, 7];
    let (side, cell) = (240, 6);
    let mut glow = glow(side, cell, &SERIAL);
    glow.clear();
    glow.gather((120 * 256, 120 * 256), [u16::MAX; 3]);
    glow.spread(&SERIAL);
    let width = usize::try_from(side).expect("small");
    let mut cells = alloc::vec![[0; 3]; glow.across()];
    let mut whole = alloc::vec![[0; 3]; width];
    glow.row(20, 0..width, &mut cells, &mut whole);
    assert!(whole.iter().any(|light| *light != [0; 3]), "the row glows");
    let mut part = alloc::vec![MARK; width];
    glow.row(20, 50..170, &mut cells, &mut part);
    assert_eq!(part[..120], whole[50..170]);
    assert!(
        part[120..].iter().all(|light| *light == MARK),
        "nothing past"
    );
    glow.row(20, 200..width + 1, &mut cells, &mut part);
    assert!(part.iter().all(|light| *light == [0; 3]), "past the frame");
}

/// The halo spread a band of rows at a time across cores, whichever band runs
/// first and when they run at once, is the halo spread whole, cell for cell.
#[test]
fn a_halo_spread_in_bands_is_the_halo_spread_whole() {
    let spread = |runner: &dyn JobRunner| {
        let mut glow = glow(600, 5, runner);
        glow.clear();
        let mut rng = NonCryptoRng::seed_from_u64(0x9E37_79B9_7F4A_7C15);
        let mut at = || u32::try_from(rng.next_below(600 * 256)).expect("small");
        for _ in 0..3_000 {
            let (x, y) = (at(), at());
            glow.gather((x, y), [9_000, 3_000, 12_000]);
        }
        glow.spread(runner);
        (glow.halo, glow.haze, glow.blocks.len())
    };
    let (whole, haze, bands) = spread(&SERIAL);
    assert_eq!(bands, 1);
    assert!(whole.iter().any(|cell| cell.r > 0), "something to spread");
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

/// A haze cell is the mean of the halo cells it gathers, a block the halo's
/// edge cuts short as much as a whole one.
#[test]
fn a_haze_cell_is_the_mean_of_the_cells_it_gathers() {
    let (across, down) = (6, 7);
    let level = Pixel16 {
        r: 900,
        g: 300,
        b: 1_200,
        a: Pixel16::FULL,
    };
    let halo = alloc::vec![level; across * down];
    let haze_across = across.div_ceil(HAZE_CELLS);
    let mut haze = alloc::vec![Pixel16::BLACK; haze_across * down.div_ceil(HAZE_CELLS)];
    gather_haze(&halo, across, &mut haze, haze_across);
    assert!(haze.iter().all(|cell| *cell == level), "{haze:?}");
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
