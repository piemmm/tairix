//! The light within a bank of cloud: what of the sun reaches each place of
//! it, and the light its cloud scatters again and again there, found by
//! following the light itself.
//!
//! A bank's light lies in levels about the eye, each twice the last's breadth
//! in as many voxels, so a voxel spans about as many pixels far off as near.
//! Each voxel holds the cloud's extinction averaged over it and the optical
//! depth from it toward the sun, swept slice by slice along the sun's way
//! from the sunward face, the next coarser level answering beyond.
//!
//! Photons are followed through the bank's own cloud, never the voxels'
//! average of it, so a cloud traps light as its own thickness has it however
//! coarse the voxels its light is gathered in: averaged over a voxel broader
//! than itself, a heap would be spread thin and let most of its light go.
//! The sun's photons enter the bank's top along its beam, the sky's from
//! above; each meets the cloud where it truly stands, drawn against a bound
//! of the extinction and kept as often as the cloud there has it (Woodcock
//! et al., "Techniques used in the GEM code for Monte Carlo neutronics
//! calculations", 1965). It scatters isotropically through the extinction
//! scaled by `1 − g` — the similarity principle, which keeps how far light
//! diffuses (van de Hulst, "Multiple Light Scattering", 1980) — and off the
//! ground as its albedo has it, until it leaves by the top. The track it
//! lays through a voxel is that voxel's share of the diffuse light's
//! fluence, the track's direction its share of the net flux, and what leaves
//! by the floor having scattered is the light the bank sheds below. A voxel's
//! bound is none where the weather over it leaves no room for cloud, so a
//! photon crosses clear air without reading it.
//!
//! Where the bank holds heaps, each level within its resolution's reach holds
//! its light again in fine cells within every voxel holding cloud, each
//! voxel's track shared out among the fine cells it crosses, so the light just
//! inside a heap's surface is held as finely as its turrets shade it. A read
//! takes the fine cells about it where they are held and the voxels for the
//! rest. Every tally is an exact integer, so the light comes out the same
//! however many cores follow the photons, in whatever order.

use alloc::vec::Vec;
use core::f64::consts::{PI, TAU};
use core::ops::Range;
use core::sync::atomic::{AtomicU64, Ordering};

use tairix_parallel::JobRunner;
use tairix_util::{fallible, mathf};

use super::form::Edges;
use super::{sunlight_at, Cloudbank, Holding, Reader, RIM};
use crate::band;
use crate::noise::smoothstep;
use crate::sample::{mix32, unit};
use crate::vector::{cell_of, real, single, whole, Vec3};

/// How finely a bank's light is solved.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Resolution {
    /// Voxels along each side of each level, and the finest level's half
    /// breadth either way of the eye.
    pub(crate) cells: usize,
    pub(crate) finest: f64,
    /// How many voxels' worth of photon track each voxel holding cloud is
    /// lit by: photons are followed through a level a round at a time until
    /// they have laid so much.
    pub(crate) tracks: f64,
    /// How many times finer than its voxels each level no broader than
    /// `reach` either way of the eye holds its diffuse light, within its
    /// voxels holding cloud, every fine cell lit by as much track as a
    /// voxel, where the bank holds heaps and a fine cell spans a pixel: the
    /// light just inside a heap's surface varies within a transport mean free
    /// path, which the voxels hold only in part, while past the reach its
    /// cloud stands low over the horizon behind miles of air, and its voxels
    /// hold its light.
    pub(crate) fine: usize,
    pub(crate) reach: f64,
}

/// The tallest a voxel stands, so even a coarse level holds a cloud's base,
/// body and top apart.
const TALLEST: f64 = 100.0;
/// The shares of a level's photons given to the sun's light and the sky's:
/// the sun's the more, as many cross the bank between its clouds and light
/// them only again off the ground.
const SHARES: [f64; 2] = [0.75, 0.25];
/// The salts each kind's places over the top are spread by, so the two runs
/// are unalike.
const SALTS: [u32; 2] = [0x2d, 0x51];
/// The footprint photons read the cloud through, in metres: turrets as fine
/// as some quarter of the droplets' scaled free path, over which the light
/// they carry diffuses, held; finer ones settled to their mean.
const FOOTPRINT: f64 = 16.0;
/// What a voxel's breadth of track tallies as: integers fine enough that
/// rounding each track loses nothing a picture shows.
const FIXED: f64 = 16_777_216.0;
/// The most steps — a voxel crossed, or a place the cloud might be met — a
/// photon is followed through before it is let go, so deep in cloud that it
/// would only ever have lit further in.
const MOST_STEPS: u32 = 1 << 14;
/// Photons in a batch, and batches a core follows in a unit: some
/// milliseconds' work, in pieces small enough that a photon wandering long
/// leaves no core idle behind it.
const PHOTON_BATCH: usize = 16;
const BATCHES: usize = 16;
/// Photons sent through a level in each round, for each of its voxels
/// holding cloud, and the most rounds sent: each a fresh draw of every
/// photon, so a level's light is their mean however many it took. A coarse
/// level's photons each light little more than the cloud they meet, so its
/// rounds end here rather than at its track: some four photons a voxel
/// holding cloud.
const ROUND: f64 = 0.25;
const MOST_ROUNDS: u32 = 16;
/// The weight below which a photon is played for: kept as often as its
/// weight against this, and then at this weight.
const LEAST_WEIGHT: f64 = 1.0 / 32.0;
/// The deepest optical depth toward the sun a voxel holds, past which no sun
/// reaches; and the steps its sixteen bits hold it in.
const DEEPEST: f64 = 50.0;
const DEPTH_SCALE: f64 = DEEPEST / 65_535.0;
/// Rows of voxels a core fills in a unit: of voxelling, a voxel some tenths
/// of a microsecond; and of a field's pass, some nanoseconds. And rows of a
/// level's columns it finds what the weather holds over, a column up to a
/// microsecond.
const VOXEL_ROWS: usize = 24;
const FIELD_ROWS: usize = 256;
const STAND_ROWS: usize = 8;
/// Cells of one tally a core lays at nought in a unit: a megabyte of fresh
/// memory.
const TALLY_CELLS: usize = 1 << 17;
/// The layers one word of a column's layer bits holds.
const LAYER_BITS: usize = u64::BITS as usize;

/// How heaped a bank's most heaped deck must stand for its light to be held
/// finely: a heap's turrets shade one another down to the scale the fine
/// cells hold, while a sheet's relief is a third of a heap's, and an
/// overcast that fills every voxel of its layers would spend the fine
/// light's photons on a grey ceiling.
const HEAPED: f64 = 0.5;
/// Blocks of fine cells a core makes a field over in a unit, and in a band.
const FIELD_BLOCKS: usize = 2048;
const BAND_BLOCKS: usize = 64;

/// Voxel columns along each side of a cell of a level's floor, over which the
/// light its cloud sheds below and the sky it leaves open are taken: a point
/// beneath sees the cloud bases about it, not one column's.
const FLOOR: usize = 4;

/// The kinds of photon, by where their light comes from.
const SUN: usize = 0;
const SKY: usize = 1;
/// The colours a voxel's diffuse light is held in: the sun's, the ground's
/// light being the sun's off a grey ground; and the sky's.
const SUNLIT: usize = 0;
const SKYLIT: usize = 1;

/// The fields a level's photons' tallies are made into — the fluence of
/// each colour, then each axis of the net flux — and the passes each takes:
/// gathered, then smoothed east-west, north-south and up-down, every pass
/// by rows. The flux, a difference of tracks running opposite ways, is the
/// noisier, and is smoothed twice over: still within the transport mean
/// free path it varies over.
const FLUENCES: usize = 2;
const FLUXES: usize = 3;
const PASSES: usize = 4;
const SMOOTHINGS: [usize; 2] = [1, 2];
const FIELD_PASSES: usize = FLUENCES * PASSES + FLUXES * (1 + 3 * SMOOTHINGS[1]);
const _: () = assert!(PASSES == 1 + 3 * SMOOTHINGS[0]);

/// The shape of a level: its half breadth either way of the bank's middle,
/// its voxels along each side and up, a voxel's breadth and height, and how
/// many times finer than its voxels it holds its light where cloud stands,
/// one where it holds it in its voxels alone.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
struct Grid {
    half: f64,
    cells: usize,
    layers: usize,
    breadth: f64,
    height: f64,
    fine: usize,
}

impl Grid {
    const fn voxels(self) -> usize {
        self.cells * self.cells * self.layers
    }

    /// The fine cells a voxel holding cloud is held as.
    const fn fine_cells(self) -> usize {
        self.fine * self.fine * self.fine
    }

    /// The words a column's layers take a bit each of.
    const fn layer_words(self) -> usize {
        self.layers.div_ceil(LAYER_BITS)
    }

    const fn rows(self) -> usize {
        self.cells * self.layers
    }

    const fn index(self, (i, k, j): (usize, usize, usize)) -> usize {
        (k * self.cells + j) * self.cells + i
    }

    /// Where voxel `(i, k, j)`'s middle lies, `(x, altitude, z)`.
    fn middle(self, bank: &Cloudbank, (i, k, j): (usize, usize, usize)) -> (f64, f64, f64) {
        (
            bank.centre.0 - self.half + (real(i) + 0.5) * self.breadth,
            bank.floor + (real(k) + 0.5) * self.height,
            bank.centre.1 - self.half + (real(j) + 0.5) * self.breadth,
        )
    }

    /// Where `(x, altitude, z)` lies among the middles of cells `per` to a
    /// voxel along each axis, counted in those cells from the first and
    /// clamped onto them; `None` off the square.
    fn place_in(
        self,
        bank: &Cloudbank,
        (x, altitude, z): (f64, f64, f64),
        per: usize,
    ) -> Option<(f64, f64, f64)> {
        let (east, north) = (x - bank.centre.0 + self.half, z - bank.centre.1 + self.half);
        let span = 2.0 * self.half;
        if !((0.0..span).contains(&east) && (0.0..span).contains(&north)) {
            return None;
        }
        let (fine, last) = (real(per), |count: usize| {
            real((count * per).saturating_sub(1))
        });
        Some((
            (east / self.breadth * fine - 0.5).clamp(0.0, last(self.cells)),
            ((altitude - bank.floor) / self.height * fine - 0.5).clamp(0.0, last(self.layers)),
            (north / self.breadth * fine - 0.5).clamp(0.0, last(self.cells)),
        ))
    }

    /// Where `at` lies among the voxels' middles.
    fn place(self, bank: &Cloudbank, at: (f64, f64, f64)) -> Option<(f64, f64, f64)> {
        self.place_in(bank, at, 1)
    }

    /// The eight cells about `place`, of cells `per` to a voxel along each
    /// axis, and the weight each is read with.
    fn cube(self, place: (f64, f64, f64), per: usize) -> [((usize, usize, usize), f64); 8] {
        let ((i, fu), (k, fv), (j, fw)) = (cell_of(place.0), cell_of(place.1), cell_of(place.2));
        let (along, up) = (self.cells * per - 1, self.layers * per - 1);
        let (i, k, j) = (i.min(along), k.min(up), j.min(along));
        let (i1, k1, j1) = ((i + 1).min(along), (k + 1).min(up), (j + 1).min(along));
        [
            ((i, k, j), (1.0 - fu) * (1.0 - fv) * (1.0 - fw)),
            ((i1, k, j), fu * (1.0 - fv) * (1.0 - fw)),
            ((i, k1, j), (1.0 - fu) * fv * (1.0 - fw)),
            ((i1, k1, j), fu * fv * (1.0 - fw)),
            ((i, k, j1), (1.0 - fu) * (1.0 - fv) * fw),
            ((i1, k, j1), fu * (1.0 - fv) * fw),
            ((i, k1, j1), (1.0 - fu) * fv * fw),
            ((i1, k1, j1), fu * fv * fw),
        ]
    }

    /// The eight voxels about a place, and the weight each is read with.
    fn corners(self, place: (f64, f64, f64)) -> [(usize, f64); 8] {
        self.cube(place, 1)
            .map(|(voxel, weight)| (self.index(voxel), weight))
    }

    /// The cells along each side of the level's floor.
    fn floor_cells(self) -> usize {
        (self.cells / FLOOR).max(1)
    }

    /// A map over a grid `per` voxel columns a cell, read bilinearly over
    /// `(x, z)` between its cells' middles, `none` for a cell it lacks;
    /// `None` off the square.
    fn column(
        self,
        bank: &Cloudbank,
        (map, per): (&[f32], usize),
        at: (f64, f64),
        none: f64,
    ) -> Option<f64> {
        let cells = (self.cells / per.max(1)).max(1);
        let size = 2.0 * self.half / real(cells);
        let (east, north) = (
            at.0 - bank.centre.0 + self.half,
            at.1 - bank.centre.1 + self.half,
        );
        let span = 2.0 * self.half;
        if !((0.0..span).contains(&east) && (0.0..span).contains(&north)) {
            return None;
        }
        let last = real(cells - 1);
        let middle = |offset: f64| cell_of((offset / size - 0.5).clamp(0.0, last));
        let ((i, fu), (j, fw)) = (middle(east), middle(north));
        let (i, j) = (i.min(cells - 1), j.min(cells - 1));
        let (i1, j1) = ((i + 1).min(cells - 1), (j + 1).min(cells - 1));
        let at = |i: usize, j: usize| {
            map.get(j * cells + i)
                .map_or(none, |&value| f64::from(value))
        };
        let lerp = |a: f64, b: f64, t: f64| a + (b - a) * t;
        Some(lerp(
            lerp(at(i, j), at(i1, j), fu),
            lerp(at(i, j1), at(i1, j1), fu),
            fw,
        ))
    }
}

/// A grid of values held in sixteen bits each, scaled to its own range.
#[derive(Clone, Debug, Default)]
struct Packed {
    scale: f64,
    codes: Vec<u16>,
    signed: bool,
}

impl Packed {
    /// Room for `count` values `scale` apart, signed or not; `None` when the
    /// heap will not hold it.
    fn reserved(count: usize, scale: f64, signed: bool) -> Option<Self> {
        Some(Self {
            scale,
            codes: fallible::filled(count, 0)?,
            signed,
        })
    }

    /// The steps a field whose greatest magnitude is `most` is held in.
    fn scale_for(most: f64, signed: bool) -> f64 {
        let top = if signed { 32_767.0 } else { 65_535.0 };
        if most > 0.0 {
            most / top
        } else {
            1.0
        }
    }

    fn code(&self, value: f64) -> u16 {
        let step = mathf::round_i32(value / self.scale);
        if self.signed {
            i16::try_from(step.clamp(-32_767, 32_767))
                .unwrap_or(0)
                .cast_unsigned()
        } else {
            u16::try_from(step.clamp(0, 65_535)).unwrap_or(0)
        }
    }

    fn at(&self, index: usize) -> f64 {
        let code = self.codes.get(index).copied().unwrap_or(0);
        let step = if self.signed {
            f64::from(code.cast_signed())
        } else {
            f64::from(code)
        };
        step * self.scale
    }

    /// The values about a place, weighed as `corners` has them.
    fn read(&self, corners: &[(usize, f64); 8]) -> f64 {
        if self.codes.is_empty() {
            return 0.0;
        }
        corners
            .iter()
            .map(|&(index, weight)| self.at(index) * weight)
            .sum()
    }
}

/// The voxels of a level holding cloud, each held as a block of its fine
/// cells: each column's runs of them, laid column by column.
#[derive(Clone, Debug, Default)]
struct Runs {
    /// Each column's first run and how many it has, and its first block.
    columns: Vec<(u32, u32)>,
    starts: Vec<u32>,
    /// Every column's runs, as their first layer and how many layers.
    spans: Vec<(u16, u16)>,
    blocks: usize,
}

impl Runs {
    /// The runs of `grid`'s voxels holding cloud, as `layered` has each
    /// column's layers, with the voxel each block holds; `None` when the
    /// heap will not hold them.
    fn holding(grid: Grid, layered: &[u64]) -> Option<(Self, Vec<u32>)> {
        let (cells, layers, words) = (grid.cells, grid.layers, grid.layer_words());
        // Each run of layers holding cloud up column `(i, j)`, its first and
        // its end.
        let each = |(i, j): (usize, usize), visit: &mut dyn FnMut(usize, usize)| {
            let column = j * cells + i;
            let bits = layered
                .get(column * words..(column + 1) * words)
                .unwrap_or(&[]);
            let mut k = 0;
            while let Some(first) = next_bit(bits, k, true) {
                let end = next_bit(bits, first, false).map_or(layers, |end| end.min(layers));
                visit(first, end);
                k = end;
            }
        };
        let (mut runs_held, mut blocks) = (0, 0);
        for column in 0..cells * cells {
            each((column % cells, column / cells), &mut |first, end| {
                runs_held += 1;
                blocks += end - first;
            });
        }
        let mut columns = fallible::filled(cells * cells, (0, 0))?;
        let mut starts = fallible::filled(cells * cells, 0)?;
        let (mut spans, mut voxels) = (Vec::new(), Vec::new());
        (fallible::reserve(&mut spans, runs_held) && fallible::reserve(&mut voxels, blocks))
            .then_some(())?;
        let mut fits = true;
        for (column, (span, start)) in columns.iter_mut().zip(&mut starts).enumerate() {
            let (i, j) = (column % cells, column / cells);
            *start = u32::try_from(voxels.len()).ok()?;
            span.0 = u32::try_from(spans.len()).ok()?;
            each((i, j), &mut |first, end| {
                let (Ok(low), Ok(count)) = (u16::try_from(first), u16::try_from(end - first))
                else {
                    fits = false;
                    return;
                };
                spans.push((low, count));
                span.1 += 1;
                voxels
                    .extend((first..end).filter_map(|k| u32::try_from(grid.index((i, k, j))).ok()));
            });
        }
        // Every voxel of a level is counted within 32 bits and every layer
        // within 16, as its blocks and runs are.
        (fits && voxels.len() == blocks && u32::try_from(blocks).is_ok()).then_some(())?;
        Some((
            Self {
                columns,
                starts,
                spans,
                blocks,
            },
            voxels,
        ))
    }

    /// The block voxel `(i, k, j)` is held as, if it is.
    fn block(&self, grid: Grid, (i, k, j): (usize, usize, usize)) -> Option<usize> {
        let column = j * grid.cells + i;
        let (first, count) = *self.columns.get(column)?;
        let first = usize::try_from(first).ok()?;
        let mut block = usize::try_from(*self.starts.get(column)?).ok()?;
        for &(low, layers) in self
            .spans
            .get(first..first + usize::try_from(count).ok()?)?
        {
            let (low, layers) = (usize::from(low), usize::from(layers));
            if (low..low + layers).contains(&k) {
                return Some(block + k - low);
            }
            block += layers;
        }
        None
    }

    /// The fine cell `(i, k, j)`, counted in fine cells, is held as, if it
    /// is.
    fn fine(&self, grid: Grid, (i, k, j): (usize, usize, usize)) -> Option<usize> {
        let fine = grid.fine;
        let block = self.block(grid, (i / fine, k / fine, j / fine))?;
        Some(block * grid.fine_cells() + fine_within(fine, (i % fine, k % fine, j % fine)))
    }
}

/// The first layer from `from` up whose bit in `bits` is `set`, if any.
fn next_bit(bits: &[u64], from: usize, set: bool) -> Option<usize> {
    let mut word = from / LAYER_BITS;
    let mut mask = u64::MAX << (from % LAYER_BITS);
    while let Some(&value) = bits.get(word) {
        let found = mask & if set { value } else { !value };
        if found != 0 {
            return Some(word * LAYER_BITS + usize::try_from(found.trailing_zeros()).ok()?);
        }
        word += 1;
        mask = u64::MAX;
    }
    None
}

/// Where fine cell `(i, k, j)` of a voxel `fine` fine cells a side lies in
/// its block.
const fn fine_within(fine: usize, (i, k, j): (usize, usize, usize)) -> usize {
    (k * fine + j) * fine + i
}

/// Where the `within`th fine cell of the block holding voxel `voxel` lies,
/// counted in fine cells.
fn fine_cell(grid: Grid, voxel: u32, within: usize) -> (usize, usize, usize) {
    let (cells, fine, voxel) = (grid.cells, grid.fine, usize::try_from(voxel).unwrap_or(0));
    let (i, j, k) = (
        voxel % cells,
        voxel / cells % cells,
        voxel / (cells * cells),
    );
    (
        i * fine + within % fine,
        k * fine + within / (fine * fine),
        j * fine + within / fine % fine,
    )
}

/// A level's diffuse light held finely within its voxels holding cloud: the
/// fluence of each colour and the net flux at each fine cell's middle.
#[derive(Clone, Debug, Default)]
struct Fine {
    runs: Runs,
    fluence: [Packed; 2],
    flux: [Packed; 3],
}

/// One level of a bank's light.
#[derive(Clone, Debug, Default)]
pub(super) struct Volume {
    grid: Grid,
    /// At each voxel's middle: the optical depth toward the sun; the diffuse
    /// light's fluence of each colour; and its net flux, in luminance. And
    /// the same light held finely where cloud stands.
    depth: Packed,
    fluence: [Packed; 2],
    flux: [Packed; 3],
    fine: Fine,
    /// Over each cell of its floor: how much of the sky overhead it leaves
    /// open, and the irradiance of each colour its cloud sheds down; and over
    /// each column what of the sunlight the bank above lets through at its
    /// ceiling.
    open: Vec<f32>,
    shed: [Vec<f32>; 2],
    above: Vec<f32>,
}

impl Volume {
    /// What the level holds at `at`: its diffuse light from the fine cells
    /// about it where they are held, and from its voxels for the share of
    /// the fine cells that are not; `None` off its square.
    fn reading(&self, bank: &Cloudbank, at: (f64, f64, f64)) -> Option<Reading> {
        let grid = self.grid;
        let corners = grid.corners(grid.place(bank, at)?);
        let coarse = |fluence: &[Packed; 2], flux: &[Packed; 3], corners: &[(usize, f64); 8]| {
            (
                [fluence[SUNLIT].read(corners), fluence[SKYLIT].read(corners)],
                Vec3::new(
                    flux[0].read(corners),
                    flux[1].read(corners),
                    flux[2].read(corners),
                ),
            )
        };
        let mut fine = [(0, 0.0); 8];
        let mut held = 0.0;
        if self.fine.runs.blocks > 0 {
            for (slot, (cell, weight)) in fine
                .iter_mut()
                .zip(grid.cube(grid.place_in(bank, at, grid.fine)?, grid.fine))
            {
                if let Some(index) = self.fine.runs.fine(grid, cell) {
                    *slot = (index, weight);
                    held += weight;
                }
            }
        }
        let (fluence, flux) = if held <= 0.0 {
            coarse(&self.fluence, &self.flux, &corners)
        } else {
            let (fluence, flux) = coarse(&self.fine.fluence, &self.fine.flux, &fine);
            if held >= 1.0 - 1e-9 {
                (fluence, flux)
            } else {
                let (wide, across) = coarse(&self.fluence, &self.flux, &corners);
                let rest = 1.0 - held;
                (
                    [fluence[0] + rest * wide[0], fluence[1] + rest * wide[1]],
                    flux + across * rest,
                )
            }
        };
        Some(Reading {
            depth: self.depth.read(&corners),
            fluence,
            flux,
        })
    }
}

/// What a bank's light holds at a place: the optical depth toward the sun,
/// the diffuse light's fluence of each colour and its net flux.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub(super) struct Reading {
    pub(super) depth: f64,
    pub(super) fluence: [f64; 2],
    pub(super) flux: Vec3,
}

impl Reading {
    fn lerp(self, other: Self, t: f64) -> Self {
        let mix = |a: f64, b: f64| a + (b - a) * t;
        Self {
            depth: mix(self.depth, other.depth),
            fluence: [
                mix(self.fluence[0], other.fluence[0]),
                mix(self.fluence[1], other.fluence[1]),
            ],
            flux: self.flux.lerp(other.flux, t),
        }
    }
}

/// How far a level's light is solved.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
enum Stage {
    /// Its levels not yet laid about the eye.
    #[default]
    Unlaid,
    /// Level by level from the coarsest in: what the weather holds over its
    /// columns, by rows of them; its voxels, by rows; the sun's optical
    /// depth, a slice at a time from the sunward face; the bank above's light
    /// at its ceiling, by rows of columns; its tallies laid at nought, a
    /// share of each at a time; the photons followed, by batches, round on
    /// round; each tally made a field, a pass at a time by rows, and then
    /// each fine tally, by blocks; and its depth and column maps, by rows.
    Stands(usize, usize),
    Voxels(usize, usize),
    Sweep(usize, usize),
    Above(usize, usize),
    Tallies(usize),
    Photons(usize, usize),
    Fields(usize, usize, usize),
    Fine(usize, usize, usize),
    Maps(usize, usize),
    Done,
}

/// A bank's light: its levels, finest first, and the one being solved.
#[derive(Clone, Debug, Default)]
pub(super) struct Lights {
    pub(super) volumes: Vec<Volume>,
    resolution: Option<Resolution>,
    solving: Option<Solving>,
    stage: Stage,
}

/// What a level holds while it is solved.
#[derive(Clone, Debug, Default)]
struct Solving {
    volume: Volume,
    /// Each voxel's extinction, averaged over it, and optical depth toward
    /// the sun; and the most extinction any of its cloud holds, which
    /// photons meet it against, from what each deck's weather holds over
    /// each column.
    sigma: Vec<f32>,
    depth: Vec<f32>,
    bounds: Vec<f32>,
    stands: Vec<[Holding; 2]>,
    /// Where its light is held finely, which of each column's layers hold
    /// cloud, a bit a layer, until its runs are found; and the voxel each
    /// block of its fine light holds.
    layered: Vec<u64>,
    voxels: Vec<u32>,
    /// How many voxels of each row hold cloud.
    cloudy: Vec<u32>,
    plan: Plan,
    tally: Tally,
    /// A field being made, its copy being smoothed, and its greatest
    /// magnitude.
    field: Vec<f32>,
    smoothed: Vec<f32>,
    most: f64,
}

/// A level's photons: how many of each kind are followed in a round, the
/// power each carries and the power its tallies are counted in; the
/// brightest sunlight in the bank, which the sun's photons' weights are
/// taken against; the round being followed, and how many voxels' track every
/// round so far has laid.
#[derive(Copy, Clone, Debug, Default)]
struct Plan {
    counts: [u64; 2],
    power: [f64; 2],
    reference: f64,
    brightest: f64,
    round: u32,
    laid: u64,
}

/// The integer tallies photons lay down as they are followed: over the
/// voxels, over the fine cells of those holding cloud, and over the floor;
/// and how many cells each of the three holds once laid.
#[derive(Debug, Default)]
struct Tally {
    voxels: Sums,
    fine: Sums,
    shed: [Vec<AtomicU64>; 2],
    cells: [usize; 3],
}

/// Tallies of each field over some cells: the fluence of each colour, then
/// each axis of the net flux.
#[derive(Debug, Default)]
struct Sums {
    fluence: [Vec<AtomicU64>; 2],
    flux: [Vec<AtomicU64>; 3],
}

impl Sums {
    /// Room for tallies over `cells`, none laid yet; `None` when the heap
    /// will not hold it.
    fn reserved(cells: usize) -> Option<Self> {
        Some(Self {
            fluence: [room(cells)?, room(cells)?],
            flux: [room(cells)?, room(cells)?, room(cells)?],
        })
    }

    /// Each field's tally.
    fn each_mut(&mut self) -> impl Iterator<Item = &mut Vec<AtomicU64>> {
        self.fluence.iter_mut().chain(&mut self.flux)
    }

    /// Field `field`'s tally.
    fn field(&self, field: usize) -> Option<&Vec<AtomicU64>> {
        match field.checked_sub(FLUENCES) {
            None => self.fluence.get(field),
            Some(axis) => self.flux.get(axis),
        }
    }

    /// Field `field`'s tally, to be let go once its field is made.
    fn field_mut(&mut self, field: usize) -> Option<&mut Vec<AtomicU64>> {
        match field.checked_sub(FLUENCES) {
            None => self.fluence.get_mut(field),
            Some(axis) => self.flux.get_mut(axis),
        }
    }
}

impl Clone for Tally {
    /// A level's tallies are its solving's alone and never copied: a clone
    /// holds none.
    fn clone(&self) -> Self {
        Self::default()
    }
}

/// Room for `count` tallies; `None` when the heap will not hold it.
fn room(count: usize) -> Option<Vec<AtomicU64>> {
    let mut values = Vec::new();
    values.try_reserve_exact(count).ok()?;
    Some(values)
}

impl Tally {
    /// Room for `volume`'s tallies over its voxels, the fine cells of its
    /// runs and its floor, none laid yet; `None` when the heap will not hold
    /// it.
    fn reserved(volume: &Volume) -> Option<Self> {
        let grid = volume.grid;
        let (voxels, fine) = (grid.voxels(), volume.fine.runs.blocks * grid.fine_cells());
        let columns = grid.floor_cells() * grid.floor_cells();
        Some(Self {
            voxels: Sums::reserved(voxels)?,
            fine: Sums::reserved(fine)?,
            shed: [room(columns)?, room(columns)?],
            cells: [voxels, fine, columns],
        })
    }

    /// Each tally, and how many cells it holds once laid.
    fn each_mut(&mut self) -> impl Iterator<Item = (&mut Vec<AtomicU64>, usize)> {
        let [voxels, fine, columns] = self.cells;
        let over_voxels = self.voxels.each_mut().map(move |values| (values, voxels));
        let over_fine = self.fine.each_mut().map(move |values| (values, fine));
        let shed = self.shed.iter_mut().map(move |values| (values, columns));
        over_voxels.chain(over_fine).chain(shed)
    }

    /// Lay up to [`TALLY_CELLS`] more cells at nought for each of `runner`'s
    /// cores, each in a tally of its own still short of its cells and within
    /// the room reserved for it; whether every one holds all its cells.
    fn lay(&mut self, runner: &dyn JobRunner) -> bool {
        let unlaid = self
            .each_mut()
            .filter(|(values, cells)| values.len() < *cells)
            .take(runner.width().max(1));
        tairix_parallel::for_each_drawn(runner, unlaid, &|(values, cells)| {
            let more = cells.saturating_sub(values.len()).min(TALLY_CELLS);
            values.extend((0..more).map(|_| AtomicU64::new(0)));
        });
        self.each_mut().all(|(values, cells)| values.len() >= cells)
    }
}

/// The most one add to a tally holds whatever its sign, within an `i32`: a
/// photon's gathered track goes to its tallies before it grows past it.
const MOST_ADD: f64 = 2.0e9;

/// Add `value`, rounded to a whole, to tally `index`, wrapping, so a signed
/// sum is held as its two's complement.
fn add(tally: &[AtomicU64], index: usize, value: f64) {
    if let Some(slot) = tally.get(index) {
        let step = i64::from(mathf::round_i32(value.clamp(-MOST_ADD, MOST_ADD)));
        slot.fetch_add(step.cast_unsigned(), Ordering::Relaxed);
    }
}

/// Tally `index`'s sum, the signed whole it is: exact to 2⁵³, far past any
/// a level lays down.
fn summed(tally: &[AtomicU64], index: usize) -> f64 {
    let bits = tally
        .get(index)
        .map_or(0, |slot| slot.load(Ordering::Relaxed).cast_signed());
    let high = i32::try_from(bits >> 32).unwrap_or(0);
    let low = u32::try_from(bits & 0xffff_ffff).unwrap_or(0);
    f64::from(high) * 4_294_967_296.0 + f64::from(low)
}

impl Lights {
    /// Light to be solved as finely as `resolution` has it.
    pub(super) fn new(resolution: Resolution) -> Self {
        Self {
            resolution: Some(resolution),
            ..Self::default()
        }
    }

    /// Lay the levels about `bank`'s middle, out to cover its coarsest level
    /// of weather, their solving started afresh, their light held finely
    /// only where a fine cell spans at least a pixel `pixel` radians across;
    /// `None` when the heap will not hold them.
    pub(super) fn lay(&mut self, bank: &Cloudbank, pixel: f64) -> Option<()> {
        let resolution = self.resolution?;
        let reach = bank.levels.last()?.half;
        let cells = resolution.cells.max(4);
        // A level's fine cells, a half breadth's `2 / (cells · fine)` across,
        // stand no nearer than the finer level's edge at half that breadth.
        let spanned = 4.0 / real(cells * resolution.fine.max(1)) >= pixel;
        let heaped = bank.decks.iter().flatten().any(|deck| deck.heap >= HEAPED);
        let span = bank.ceiling - bank.floor;
        let mut volumes = Vec::new();
        let mut half = resolution.finest.min(reach);
        loop {
            let breadth = 2.0 * half / real(cells);
            let layers = whole(mathf::ceil(span / breadth.min(TALLEST))).max(2);
            volumes.try_reserve(1).ok()?;
            volumes.push(Volume {
                grid: Grid {
                    half,
                    cells,
                    layers,
                    breadth,
                    height: span / real(layers),
                    fine: if heaped && spanned && half <= resolution.reach {
                        resolution.fine.max(1)
                    } else {
                        1
                    },
                },
                ..Volume::default()
            });
            if half >= reach {
                break;
            }
            half *= 2.0;
        }
        self.stage = Stage::Stands(volumes.len() - 1, 0);
        self.volumes = volumes;
        self.solving = None;
        Some(())
    }

    /// The units of solving done and to do, each a core's work at its stage.
    pub(super) fn units(&self) -> (f64, f64) {
        let tracks = self.resolution.map_or(0.0, |resolution| resolution.tracks);
        let per_level = |volume: &Volume| {
            let grid = volume.grid;
            let rows = real(grid.rows());
            // About a tenth of a level's voxels hold cloud on the whole; a
            // fine level's photons lay some fifty voxels' track, a coarse
            // level's too little to end its rounds before the last.
            let rounds = (tracks / (ROUND * 50.0)).min(f64::from(MOST_ROUNDS));
            let cloudy = 0.1 * real(grid.voxels());
            let fine = if grid.fine > 1 { cloudy } else { 0.0 };
            let tallied = real(grid.voxels()) + fine * real(grid.fine_cells());
            real(grid.cells) / real(STAND_ROWS)
                + rows / real(VOXEL_ROWS)
                + real(grid.layers.max(grid.cells))
                + real(FLUENCES + FLUXES) * tallied / real(TALLY_CELLS)
                + rounds * ROUND * real(grid.fine * grid.fine) * cloudy / real(PHOTON_BATCH)
                + real(FIELD_PASSES + 1) * rows / real(FIELD_ROWS)
                + real(FIELD_PASSES) * fine / real(FIELD_BLOCKS)
        };
        let total: f64 = self.volumes.iter().map(per_level).sum();
        let level = match self.stage {
            Stage::Unlaid => return (0.0, total),
            Stage::Done => return (total, total),
            Stage::Stands(level, _)
            | Stage::Voxels(level, _)
            | Stage::Sweep(level, _)
            | Stage::Above(level, _)
            | Stage::Tallies(level)
            | Stage::Photons(level, _)
            | Stage::Fields(level, ..)
            | Stage::Fine(level, ..)
            | Stage::Maps(level, _) => level,
        };
        // The levels past this one, solved first, are done.
        let done: f64 = self.volumes.iter().skip(level + 1).map(per_level).sum();
        (done, total)
    }

    /// Solve the next unit of `bank`'s light across `runner`, the bank
    /// `above` it, if any, dimming its sunlight; whether every level is
    /// solved, or `None` when the heap will not hold the work.
    pub(super) fn step(
        &mut self,
        bank: &Cloudbank,
        runner: &dyn JobRunner,
        above: Option<&Cloudbank>,
    ) -> Option<bool> {
        let width = runner.width().max(1);
        let rows_of = |solving: &Solving| solving.volume.grid.rows();
        self.stage = match self.stage {
            Stage::Unlaid => return None,
            Stage::Done => return Some(true),
            Stage::Stands(level, row) => {
                if self.solving.is_none() {
                    self.solving = Some(begin(self.volumes.get(level)?.grid)?);
                }
                let solving = self.solving.as_mut()?;
                let cells = solving.volume.grid.cells;
                let end = (row + STAND_ROWS * width).min(cells);
                stand_rows(solving, bank, row..end, runner)?;
                if end >= cells {
                    Stage::Voxels(level, 0)
                } else {
                    Stage::Stands(level, end)
                }
            }
            Stage::Voxels(level, row) => {
                let solving = self.solving.as_mut()?;
                let end = (row + VOXEL_ROWS * width).min(rows_of(solving));
                voxel_rows(solving, bank, row..end, runner)?;
                if end >= rows_of(solving) {
                    Stage::Sweep(level, 0)
                } else {
                    Stage::Voxels(level, end)
                }
            }
            Stage::Sweep(level, slice) => {
                let solving = self.solving.as_mut()?;
                let sweep = Sweep::new(solving.volume.grid, bank.sun);
                sweep.slice(solving, bank, self.volumes.get(level + 1), slice, runner)?;
                if slice + 1 >= sweep.slices {
                    Stage::Above(level, 0)
                } else {
                    Stage::Sweep(level, slice + 1)
                }
            }
            Stage::Above(level, row) => {
                let solving = self.solving.as_mut()?;
                let cells = solving.volume.grid.cells;
                let end = (row + 8 * width).min(cells);
                above_rows(solving, bank, above, row..end, runner)?;
                if end >= cells {
                    solving.tally = Tally::reserved(&solving.volume)?;
                    Stage::Tallies(level)
                } else {
                    Stage::Above(level, end)
                }
            }
            Stage::Tallies(level) => {
                let solving = self.solving.as_mut()?;
                if solving.tally.lay(runner) {
                    plan(solving, bank, self.resolution?)?;
                    Stage::Photons(level, 0)
                } else {
                    Stage::Tallies(level)
                }
            }
            Stage::Photons(level, first) => self.photons((level, first), bank, runner)?,
            Stage::Fields(level, pass, row) => {
                let solving = self.solving.as_mut()?;
                let end = (row + FIELD_ROWS * width).min(rows_of(solving));
                field_pass(solving, pass, row..end, runner)?;
                let at = |pass, row| Stage::Fields(level, pass, row);
                passed((pass, end, rows_of(solving)), at, Stage::Fine(level, 0, 0))
            }
            Stage::Fine(level, pass, block) => {
                let solving = self.solving.as_mut()?;
                let blocks = solving.volume.fine.runs.blocks;
                let end = (block + FIELD_BLOCKS * width).min(blocks);
                fine_pass(solving, pass, block..end, runner)?;
                let at = |pass, block| Stage::Fine(level, pass, block);
                passed((pass, end, blocks), at, Stage::Maps(level, 0))
            }
            Stage::Maps(level, row) => {
                let solving = self.solving.as_mut()?;
                let cells = solving.volume.grid.cells;
                let end = (row + FIELD_ROWS * width).min(rows_of(solving));
                map_rows(solving, row..end, runner)?;
                if end < rows_of(solving) {
                    Stage::Maps(level, end)
                } else {
                    self.finish(level, cells)?;
                    level
                        .checked_sub(1)
                        .map_or(Stage::Done, |finer| Stage::Stands(finer, 0))
                }
            }
        };
        Some(self.stage == Stage::Done)
    }

    /// Follow the next unit of level `level`'s photons from batch `first`
    /// across `runner`; the stage after it: on through the round, another
    /// round while the track laid falls short of what the level's cloud
    /// wants, and its fields once it does not or the rounds run out.
    fn photons(
        &mut self,
        (level, first): (usize, usize),
        bank: &Cloudbank,
        runner: &dyn JobRunner,
    ) -> Option<Stage> {
        let tracks = self.resolution?.tracks;
        let solving = self.solving.as_mut()?;
        let total = usize::try_from(solving.plan.counts.iter().sum::<u64>()).ok()?;
        let batches = total.div_ceil(PHOTON_BATCH);
        let end = (first + BATCHES * runner.width().max(1)).min(batches);
        let laid = follow(solving, bank, first..end, runner);
        solving.plan.laid = solving.plan.laid.saturating_add(laid);
        if end < batches {
            return Some(Stage::Photons(level, end));
        }
        let cloudy: f64 = solving.cloudy.iter().map(|&count| f64::from(count)).sum();
        let fine = solving.volume.grid.fine;
        let wanted = tracks * real(fine * fine) * cloudy;
        solving.plan.round += 1;
        let more = track_laid(&solving.plan) < wanted && solving.plan.round < MOST_ROUNDS;
        Some(if more && total > 0 {
            Stage::Photons(level, 0)
        } else {
            Stage::Fields(level, 0, 0)
        })
    }

    /// Level `level`, `cells` voxels a side, solved: its floor's maps made
    /// and the volume kept in its place.
    fn finish(&mut self, level: usize, cells: usize) -> Option<()> {
        let mut solved = self.solving.take()?;
        let floor = solved.volume.grid.floor_cells();
        solved.volume.open = smoothed(&open_floor(&solved, cells)?, floor)?;
        let [sunlit, skylit] = shed_floor(&solved)?;
        solved.volume.shed = [smoothed(&sunlit, floor)?, smoothed(&skylit, floor)?];
        *self.volumes.get_mut(level)? = solved.volume;
        Some(())
    }

    /// The finest level whose square holds `(x, z)`, and how far its rim
    /// there is blended into the next.
    fn finest(&self, bank: &Cloudbank, (x, z): (f64, f64)) -> Option<(usize, f64)> {
        let out = (x - bank.centre.0).abs().max((z - bank.centre.1).abs());
        let index = self
            .volumes
            .iter()
            .position(|volume| out < volume.grid.half)?;
        let half = self.volumes.get(index)?.grid.half;
        let rimmed = if index + 1 < self.volumes.len() {
            smoothstep((1.0 - RIM) * half, half, out)
        } else {
            0.0
        };
        Some((index, rimmed))
    }

    /// `read` of the finest level over `(x, z)`, its rim blended into the
    /// next level's own.
    fn blended<T>(
        &self,
        bank: &Cloudbank,
        (x, z): (f64, f64),
        read: impl Fn(&Volume) -> Option<T>,
        lerp: impl FnOnce(T, T, f64) -> T,
    ) -> Option<T> {
        let (index, rimmed) = self.finest(bank, (x, z))?;
        let fine = read(self.volumes.get(index)?)?;
        if rimmed <= 0.0 {
            return Some(fine);
        }
        Some(match self.volumes.get(index + 1).and_then(&read) {
            Some(coarse) => lerp(fine, coarse, rimmed),
            None => fine,
        })
    }

    /// What the light holds at `(x, altitude, z)`.
    pub(super) fn reading(&self, bank: &Cloudbank, at: (f64, f64, f64)) -> Option<Reading> {
        self.blended(
            bank,
            (at.0, at.2),
            |volume| volume.reading(bank, at),
            Reading::lerp,
        )
    }

    /// The breadth of the finest level's voxels over `(x, z)`, the coarsest's
    /// past every level.
    pub(super) fn breadth(&self, bank: &Cloudbank, (x, z): (f64, f64)) -> f64 {
        self.finest(bank, (x, z))
            .and_then(|(index, _)| self.volumes.get(index))
            .or_else(|| self.volumes.last())
            .map_or(0.0, |volume| volume.grid.breadth)
    }

    /// The optical depth toward the sun at `(x, altitude, z)`.
    pub(super) fn depth(&self, bank: &Cloudbank, at: (f64, f64, f64)) -> Option<f64> {
        self.blended(
            bank,
            (at.0, at.2),
            |volume| {
                let corners = volume.grid.corners(volume.grid.place(bank, at)?);
                Some(volume.depth.read(&corners))
            },
            |a, b, t| a + (b - a) * t,
        )
    }

    /// One of the levels' maps over `(x, z)`, a cell every `per` voxel
    /// columns, `none` past them.
    fn column(
        &self,
        bank: &Cloudbank,
        (map, per): (fn(&Volume) -> &[f32], usize),
        (x, z): (f64, f64),
        none: f64,
    ) -> f64 {
        self.blended(
            bank,
            (x, z),
            |volume| volume.grid.column(bank, (map(volume), per), (x, z), none),
            |a, b, t| a + (b - a) * t,
        )
        .unwrap_or(none)
    }

    /// How much of the clear sky straight overhead the bank leaves open over
    /// `(x, z)`, and the irradiance of each colour its cloud sheds down.
    pub(super) fn beneath(&self, bank: &Cloudbank, (x, z): (f64, f64)) -> (f64, [f64; 2]) {
        (
            self.column(bank, (|volume| &volume.open, FLOOR), (x, z), 1.0),
            [
                self.column(bank, (|volume| &volume.shed[SUNLIT], FLOOR), (x, z), 0.0),
                self.column(bank, (|volume| &volume.shed[SKYLIT], FLOOR), (x, z), 0.0),
            ],
        )
    }

    /// What of the sunlight the bank above lets through to `(x, altitude,
    /// z)`, read at the ceiling where the way toward the sun rises through it.
    pub(super) fn above(&self, bank: &Cloudbank, at: (f64, f64, f64)) -> f64 {
        let (x, z) = ceiling_under(bank, at);
        self.column(bank, (|volume| &volume.above, 1), (x, z), 1.0)
            .clamp(0.0, 1.0)
    }
}

/// The stage after pass `pass` of a level's fields has made them as far as
/// `end` of `count`: on through the pass, `at` the next pass from its start,
/// or `then` once the last pass is made, or at once over no cells at all.
fn passed(
    (pass, end, count): (usize, usize, usize),
    at: impl Fn(usize, usize) -> Stage,
    then: Stage,
) -> Stage {
    match (end >= count, pass + 1 >= FIELD_PASSES || count == 0) {
        (false, _) => at(pass, end),
        (true, false) => at(pass + 1, 0),
        (true, true) => then,
    }
}

/// Where the way toward `bank`'s sun from `(x, altitude, z)` rises through
/// its ceiling: straight up from it with the sun at or below the level.
fn ceiling_under(bank: &Cloudbank, (x, altitude, z): (f64, f64, f64)) -> (f64, f64) {
    let sun = bank.sun;
    if sun.y <= 1e-3 {
        return (x, z);
    }
    let rise = (bank.ceiling - altitude).max(0.0) / sun.y;
    (x + sun.x * rise, z + sun.z * rise)
}

/// `value` brought round onto `0.0..side`, as a photon leaving one side of
/// a level's square comes round on the other.
fn around(value: f64, side: f64) -> f64 {
    let wrapped = value - side * mathf::floor(value / side);
    if wrapped >= side {
        0.0
    } else {
        wrapped
    }
}

/// A level's solving begun, its maps reserved; `None` when the heap will not
/// hold them.
fn begin(grid: Grid) -> Option<Solving> {
    Some(Solving {
        sigma: fallible::filled(grid.voxels(), 0.0)?,
        depth: fallible::filled(grid.voxels(), 0.0)?,
        bounds: fallible::filled(grid.voxels(), 0.0)?,
        stands: fallible::filled(grid.cells * grid.cells, [Holding::NONE; 2])?,
        layered: if grid.fine > 1 {
            fallible::filled(grid.cells * grid.cells * grid.layer_words(), 0)?
        } else {
            Vec::new()
        },
        cloudy: fallible::filled(grid.rows(), 0)?,
        volume: Volume {
            grid,
            above: fallible::filled(grid.cells * grid.cells, 1.0)?,
            ..Volume::default()
        },
        ..Solving::default()
    })
}

/// The point over the Earth's curve at `(x, altitude, z)`.
fn point(bank: &Cloudbank, (x, altitude, z): (f64, f64, f64)) -> Vec3 {
    Vec3::new(x, bank.height_at(altitude, (x, z)), z)
}

/// Rows `rows` of the level's columns of voxels: what each deck's weather
/// holds over each.
fn stand_rows(
    solving: &mut Solving,
    bank: &Cloudbank,
    rows: Range<usize>,
    runner: &dyn JobRunner,
) -> Option<()> {
    let grid = solving.volume.grid;
    let (west, south) = (bank.centre.0 - grid.half, bank.centre.1 - grid.half);
    let stands = solving
        .stands
        .get_mut(rows.start * grid.cells..rows.end * grid.cells)?;
    band::for_each(runner, stands, (rows.start, grid.cells), &|j, band| {
        for (i, stand) in band.iter_mut().enumerate() {
            let low = (
                west + real(i) * grid.breadth,
                south + real(j) * grid.breadth,
            );
            *stand = bank.weather_over(low, (low.0 + grid.breadth, low.1 + grid.breadth));
        }
    });
    Some(())
}

/// Rows `rows` of the level's voxels: each one's extinction, averaged over it
/// where it is broader than a billow, and the most any of its cloud holds;
/// and how many in each row hold cloud.
fn voxel_rows(
    solving: &mut Solving,
    bank: &Cloudbank,
    rows: Range<usize>,
    runner: &dyn JobRunner,
) -> Option<()> {
    let grid = solving.volume.grid;
    let cells = grid.cells;
    let billow = bank.finest_billow();
    // Two samples a side where a voxel spans more than a billow, a quarter
    // of its breadth either way of its middle.
    let side = if grid.breadth > billow { 2 } else { 1 };
    let reader = Reader { bank };
    let sigma = solving
        .sigma
        .get_mut(rows.start * cells..rows.end * cells)?;
    band::for_each(runner, sigma, (rows.start, cells), &|row, band| {
        let (k, j) = (row / cells, row % cells);
        for (i, slot) in band.iter_mut().enumerate() {
            let (x, altitude, z) = grid.middle(bank, (i, k, j));
            let mut sum = 0.0;
            for sample in 0..side * side * side {
                let offset = |axis: usize, size: f64| {
                    if side == 1 {
                        0.0
                    } else if (sample >> axis) & 1 == 0 {
                        -0.25 * size
                    } else {
                        0.25 * size
                    }
                };
                let at = (
                    x + offset(0, grid.breadth),
                    altitude + offset(1, grid.height),
                    z + offset(2, grid.breadth),
                );
                sum += reader
                    .density_at(point(bank, at), at.1, Edges::Resolved(grid.breadth))
                    .map_or(0.0, |sample| sample.extinction);
            }
            *slot = single(sum / real(side * side * side));
        }
    });
    let stands = &solving.stands;
    let bounds = solving
        .bounds
        .get_mut(rows.start * cells..rows.end * cells)?;
    band::for_each(runner, bounds, (rows.start, cells), &|row, band| {
        let (k, j) = (row / cells, row % cells);
        let low = bank.floor + real(k) * grid.height;
        for (i, slot) in band.iter_mut().enumerate() {
            let over = stands
                .get(j * cells + i)
                .copied()
                .unwrap_or([Holding::NONE; 2]);
            *slot = single(bank.densest(&over, (low, low + grid.height), FOOTPRINT));
        }
    });
    if rows.end >= grid.rows() {
        solving.stands = Vec::new();
    }
    let words = grid.layer_words();
    for row in rows.clone() {
        let (k, j) = (row / cells, row % cells);
        let sigma = solving.sigma.get(row * cells..(row + 1) * cells)?;
        let mut held = 0u32;
        for (i, _) in sigma.iter().enumerate().filter(|&(_, &value)| value > 0.0) {
            held += 1;
            if let Some(word) = solving
                .layered
                .get_mut((j * cells + i) * words + k / LAYER_BITS)
            {
                *word |= 1 << (k % LAYER_BITS);
            }
        }
        *solving.cloudy.get_mut(row)? = held;
    }
    if rows.end >= grid.rows() && grid.fine > 1 {
        (solving.volume.fine.runs, solving.voxels) = Runs::holding(grid, &solving.layered)?;
        solving.layered = Vec::new();
    }
    Some(())
}

/// How the sun's optical depth is swept through a level: along the axis its
/// way crosses voxels fastest, so each step from one slice to the next lies
/// within a voxel across of the last slice's.
#[derive(Copy, Clone, Debug)]
struct Sweep {
    grid: Grid,
    /// The axis swept along, `0` east, `1` up, `2` north; how many slices it
    /// holds; whether the sun stands toward its far end; the length along
    /// the sun's way of a step from one slice to the next; and the sun.
    axis: usize,
    slices: usize,
    sunward_far: bool,
    step: f64,
    sun: Vec3,
}

impl Sweep {
    fn new(grid: Grid, sun: Vec3) -> Self {
        let rates = [
            sun.x.abs() / grid.breadth,
            sun.y.abs() / grid.height,
            sun.z.abs() / grid.breadth,
        ];
        let axis = if rates[1] >= rates[0] && rates[1] >= rates[2] {
            1
        } else if rates[0] >= rates[2] {
            0
        } else {
            2
        };
        let (size, slices, toward) = match axis {
            0 => (grid.breadth, grid.cells, sun.x),
            1 => (grid.height, grid.layers, sun.y),
            _ => (grid.breadth, grid.cells, sun.z),
        };
        Self {
            grid,
            axis,
            slices,
            sunward_far: toward >= 0.0,
            step: size / toward.abs().max(1e-12),
            sun,
        }
    }

    /// The breadths of a slice across the swept axis, in voxels.
    const fn across(&self) -> (usize, usize) {
        match self.axis {
            0 | 2 => (self.grid.cells, self.grid.layers),
            _ => (self.grid.cells, self.grid.cells),
        }
    }

    /// The voxel at `(a, b)` across slice `index` along the swept axis.
    const fn voxel(&self, index: usize, (a, b): (usize, usize)) -> (usize, usize, usize) {
        match self.axis {
            0 => (index, b, a),
            1 => (a, index, b),
            _ => (a, b, index),
        }
    }

    /// How far a step toward the sun moves across a slice, in voxels.
    fn shift(&self) -> (f64, f64) {
        let (sun, step, grid) = (self.sun, self.step, self.grid);
        match self.axis {
            0 => (sun.z * step / grid.breadth, sun.y * step / grid.height),
            1 => (sun.x * step / grid.breadth, sun.z * step / grid.breadth),
            _ => (sun.x * step / grid.breadth, sun.y * step / grid.height),
        }
    }

    /// Slice `slice`, counted from the sunward face, swept across `runner`:
    /// each voxel's depth is the depth a step toward the sun, read from the
    /// slice before it or from `coarser` beyond the level, and the
    /// extinction along that step.
    fn slice(
        &self,
        solving: &mut Solving,
        bank: &Cloudbank,
        coarser: Option<&Volume>,
        slice: usize,
        runner: &dyn JobRunner,
    ) -> Option<()> {
        let grid = self.grid;
        let index = if self.sunward_far {
            self.slices - 1 - slice
        } else {
            slice
        };
        let upstream = if self.sunward_far {
            index + 1
        } else {
            index.wrapping_sub(1)
        };
        let (across, (da, db)) = (self.across(), self.shift());
        let mut values = fallible::filled(across.0 * across.1, 0.0f32)?;
        let (sigma, depth) = (&solving.sigma, &solving.depth);
        band::for_each(runner, &mut values, (0, across.0), &|b, band| {
            for (a, slot) in band.iter_mut().enumerate() {
                let voxel = self.voxel(index, (a, b));
                let own = sigma
                    .get(grid.index(voxel))
                    .map_or(0.0, |&value| f64::from(value));
                let here = grid.middle(bank, voxel);
                // Beyond the level: the coarser level's depth, and none above
                // the ceiling or below the floor.
                let beyond = |length: f64| {
                    let at = (
                        here.0 + self.sun.x * length,
                        here.1 + self.sun.y * length,
                        here.2 + self.sun.z * length,
                    );
                    if at.1 >= bank.ceiling || at.1 <= bank.floor {
                        return 0.0;
                    }
                    coarser
                        .and_then(|volume| {
                            let corners = volume.grid.corners(volume.grid.place(bank, at)?);
                            Some(volume.depth.read(&corners))
                        })
                        .unwrap_or(0.0)
                };
                let (pa, pb) = (real(a) + da, real(b) + db);
                let within = |place: f64, count: usize| (0.0..=real(count - 1)).contains(&place);
                let value = if upstream >= self.slices {
                    beyond(0.5 * self.step) + own * 0.5 * self.step
                } else if within(pa, across.0) && within(pb, across.1) {
                    let ((a0, fa), (b0, fb)) = (cell_of(pa), cell_of(pb));
                    let (a1, b1) = ((a0 + 1).min(across.0 - 1), (b0 + 1).min(across.1 - 1));
                    let read = |values: &[f32], a: usize, b: usize| {
                        values
                            .get(grid.index(self.voxel(upstream, (a, b))))
                            .map_or(0.0, |&value| f64::from(value))
                    };
                    let blend = |values: &[f32]| {
                        let lerp = |x: f64, y: f64, t: f64| x + (y - x) * t;
                        lerp(
                            lerp(read(values, a0, b0), read(values, a1, b0), fa),
                            lerp(read(values, a0, b1), read(values, a1, b1), fa),
                            fb,
                        )
                    };
                    blend(depth) + f64::midpoint(own, blend(sigma)) * self.step
                } else {
                    // The step leaves the level across its side, its ceiling
                    // or its floor.
                    beyond(self.step) + own * self.step
                };
                *slot = single(value.min(DEEPEST));
            }
        });
        for b in 0..across.1 {
            for a in 0..across.0 {
                let at = grid.index(self.voxel(index, (a, b)));
                *solving.depth.get_mut(at)? = values.get(b * across.0 + a).copied().unwrap_or(0.0);
            }
        }
        Some(())
    }
}

/// Rows `rows` of the level's columns of what of the sunlight the bank
/// `above` lets through at its ceiling.
fn above_rows(
    solving: &mut Solving,
    bank: &Cloudbank,
    above: Option<&Cloudbank>,
    rows: Range<usize>,
    runner: &dyn JobRunner,
) -> Option<()> {
    let Some(high) = above else {
        return Some(());
    };
    let grid = solving.volume.grid;
    let cells = grid.cells;
    let columns = solving
        .volume
        .above
        .get_mut(rows.start * cells..rows.end * cells)?;
    band::for_each(runner, columns, (rows.start, cells), &|j, band| {
        for (i, slot) in band.iter_mut().enumerate() {
            let (x, _, z) = grid.middle(bank, (i, 0, j));
            *slot = single(high.shadow(point(bank, (x, bank.ceiling, z)), bank.sun));
        }
    });
    Some(())
}

/// The luminance of the sunlight reaching `at` past the bank above, before
/// its own cloud dims it, as `volume`'s map of the bank above has it.
fn sunlight(bank: &Cloudbank, volume: &Volume, at: (f64, f64, f64)) -> f64 {
    let Some(lighting) = bank.lighting.as_ref() else {
        return 0.0;
    };
    let height = ((at.1 - bank.floor) / (bank.ceiling - bank.floor)).clamp(0.0, 1.0);
    let cosine = bank.cosine(point(bank, at), at.1, lighting.toward);
    sunlight_at(lighting, height, cosine).luminance() * kept_above(bank, volume, at)
}

/// What of the sunlight the bank above lets through to `at`, as `volume`'s
/// map of it has it.
fn kept_above(bank: &Cloudbank, volume: &Volume, at: (f64, f64, f64)) -> f64 {
    volume
        .grid
        .column(bank, (&volume.above, 1), ceiling_under(bank, at), 1.0)
        .unwrap_or(1.0)
        .clamp(0.0, 1.0)
}

/// The scaled extinction photons are followed through, over `bank`'s own:
/// the similarity principle's `1 − g`.
fn scaled(bank: &Cloudbank) -> f64 {
    1.0 - bank.scattering().asymmetry
}

/// The level's photons planned: a round of them for its voxels holding
/// cloud, shared between the kinds, the sun's beam entering the top as the
/// brightest sunlight in the bank would.
fn plan(solving: &mut Solving, bank: &Cloudbank, resolution: Resolution) -> Option<()> {
    let grid = solving.volume.grid;
    // A bank no air lights sends no photons.
    let Some(lighting) = bank.lighting.as_ref() else {
        solving.plan = Plan::default();
        return Some(());
    };
    let cloudy: f64 = solving.cloudy.iter().map(|&count| f64::from(count)).sum();
    let wanted = if resolution.tracks > 0.0 {
        ROUND * real(grid.fine * grid.fine) * cloudy
    } else {
        0.0
    };
    let area = 4.0 * grid.half * grid.half;
    let brightest = lighting
        .sunlight
        .iter()
        .map(|light| light.luminance())
        .fold(0.0, f64::max);
    let powers = [
        brightest * bank.sun.y.max(0.0) * area,
        PI * lighting.above.luminance() * area,
    ];
    let mut plan = Plan {
        brightest,
        ..Plan::default()
    };
    for kind in [SUN, SKY] {
        let count = if powers[kind] > 0.0 {
            whole(wanted * SHARES[kind])
        } else {
            0
        };
        plan.counts[kind] = u64::try_from(count).ok()?;
        plan.power[kind] = if count > 0 {
            powers[kind] / real(count)
        } else {
            0.0
        };
    }
    plan.reference = plan.power.iter().copied().fold(0.0, f64::max);
    solving.plan = plan;
    Some(())
}

/// The voxels' worth of track a plan's rounds have laid, exact to far past
/// any a level lays.
fn track_laid(plan: &Plan) -> f64 {
    let (high, low) = (plan.laid >> 32, plan.laid & 0xffff_ffff);
    f64::from(u32::try_from(high).unwrap_or(u32::MAX)) * 4_294_967_296.0
        + f64::from(u32::try_from(low).unwrap_or(0))
}

/// The rounds of photons a plan's tallies hold, which their mean is over.
fn rounds(plan: &Plan) -> f64 {
    f64::from(plan.round.max(1))
}

/// A level's seed turned for round `round`, so each round's photons are a
/// fresh draw.
fn round_seed(seed: u32, round: u32) -> u32 {
    mix32(seed ^ mix32(round.wrapping_mul(0x9e37_79b9) ^ 0x2545_f491))
}

/// The seed a level's photons are drawn from: the bank's, and the level's
/// breadth.
fn level_seed(bank: &Cloudbank, grid: Grid) -> u32 {
    mix32(bank.seed() ^ mix32(mathf::round_i32(grid.half).cast_unsigned()))
}

/// A photon's own stream of draws, hashed from the level's seed and its
/// index.
struct Draws {
    key: u32,
    count: u32,
}

impl Draws {
    fn new(seed: u32, index: u64) -> Self {
        let low = u32::try_from(index & 0xffff_ffff).unwrap_or(0);
        let high = u32::try_from(index >> 32).unwrap_or(0);
        Self {
            key: mix32(seed ^ mix32(low ^ mix32(high ^ 0x9e37_79b9))),
            count: 0,
        }
    }

    fn next(&mut self) -> f64 {
        self.count = self.count.wrapping_add(1);
        unit(mix32(
            self.key ^ mix32(self.count.wrapping_mul(0x85eb_ca6b)),
        ))
    }
}

/// A unit way drawn evenly over the sphere.
fn isotropic(draws: &mut Draws) -> Vec3 {
    let rise = 1.0 - 2.0 * draws.next();
    let level = mathf::sqrt((1.0 - rise * rise).max(0.0));
    let around = TAU * draws.next();
    Vec3::new(level * mathf::cos(around), rise, level * mathf::sin(around))
}

/// A unit way drawn upward as a diffuse surface sends its light.
fn lambertian(draws: &mut Draws) -> Vec3 {
    let out = mathf::sqrt(draws.next());
    let around = TAU * draws.next();
    let rise = mathf::sqrt((1.0 - out * out).max(0.0));
    Vec3::new(out * mathf::cos(around), rise, out * mathf::sin(around))
}

/// Point `index` of a sequence spread evenly over the unit square, every run
/// of it as even as any (Roberts' R2), turned by `seed`.
fn spread(index: u64, seed: u32) -> (f64, f64) {
    const PLASTIC: f64 = 1.324_717_957_244_746;
    let (u0, v0) = (unit(mix32(seed)), unit(mix32(seed ^ 0x2545_f491)));
    let n = f64::from(u32::try_from(index).unwrap_or(u32::MAX));
    let wrap = |value: f64| value - mathf::floor(value);
    (wrap(u0 + n / PLASTIC), wrap(v0 + n / (PLASTIC * PLASTIC)))
}

/// Follow batches `drawn` of the level's round across `runner`; how many
/// voxels' track they laid.
fn follow(solving: &Solving, bank: &Cloudbank, drawn: Range<usize>, runner: &dyn JobRunner) -> u64 {
    let total = usize::try_from(solving.plan.counts.iter().sum::<u64>()).unwrap_or(0);
    tairix_parallel::fold_drawn(
        runner,
        drawn,
        0u64,
        &|batch| {
            let walker = Walker::new(solving, bank);
            let first = batch * PHOTON_BATCH;
            (first..(first + PHOTON_BATCH).min(total))
                .map(|index| u64::from(walker.send(index)))
                .sum()
        },
        &u64::saturating_add,
    )
}

/// A photon being followed: where it is, in voxels from the level's corner
/// and floor; the way it heads; its kind; its weight against its kind's
/// power; whether it is still the sun's beam, which lights the cloud past
/// what the photons gather; and whether it has scattered in the cloud since
/// it last left the ground.
#[derive(Copy, Clone, Debug)]
struct Photon {
    at: [f64; 3],
    way: Vec3,
    kind: usize,
    weight: f64,
    beam: bool,
    scattered: bool,
}

impl Photon {
    fn new(at: [f64; 3], way: Vec3, kind: usize) -> Self {
        Self {
            at,
            way,
            kind,
            weight: 1.0,
            beam: kind == SUN,
            scattered: false,
        }
    }
}

/// A photon's track through the voxel it last crossed, gathered before it is
/// tallied: a photon scattering again and again within one voxel touches its
/// tallies once.
struct Pending {
    colour: usize,
    index: Option<usize>,
    fluence: f64,
    flux: [f64; 3],
}

impl Pending {
    const fn new(colour: usize) -> Self {
        Self {
            colour,
            index: None,
            fluence: 0.0,
            flux: [0.0; 3],
        }
    }

    /// `track` laid through cell `index` of `sums` heading along `ways`.
    fn lay(&mut self, sums: &Sums, index: usize, track: f64, ways: [f64; 3]) {
        if self.index != Some(index) || self.fluence + track > MOST_ADD {
            self.flush(sums);
            self.index = Some(index);
        }
        self.fluence += track;
        for (flux, along) in self.flux.iter_mut().zip(ways) {
            *flux += track * along;
        }
    }

    /// What is gathered added to `sums`.
    fn flush(&mut self, sums: &Sums) {
        if let Some(index) = self.index.take() {
            add(&sums.fluence[self.colour], index, self.fluence);
            for (flux, gathered) in sums.flux.iter().zip(self.flux) {
                add(flux, index, gathered);
            }
        }
        self.fluence = 0.0;
        self.flux = [0.0; 3];
    }
}

/// How a photon's flight ended: in a collision, out of the top, or out of
/// the floor going down.
enum Flight {
    Collided,
    Escaped,
    Grounded,
}

/// What a photon is followed through: the level's voxels, the tallies it
/// lays down, the bank's own cloud, and the ground beneath.
struct Walker<'a> {
    solving: &'a Solving,
    bank: &'a Cloudbank,
    reader: Reader<'a>,
    grid: Grid,
    /// The floor's height above the ground.
    floor: f64,
    /// What each kind's photon adds to the tallies against the power they
    /// are counted in; the share of the extinction it is followed through
    /// and of what it meets the cloud scatters; and the ground's albedo.
    factor: [f64; 2],
    scaled: f64,
    survives: f64,
    albedo: f64,
    /// The sunlight on the open ground over the beam's through the level's
    /// top, which the sun's photons are counted in.
    grounded: f64,
    /// The seeds its photons' draws are hashed from: the round's; and the
    /// level's, which spreads each kind's places on through the rounds.
    seed: u32,
    spreading: u32,
}

impl<'a> Walker<'a> {
    fn new(solving: &'a Solving, bank: &'a Cloudbank) -> Self {
        let plan = &solving.plan;
        let spreading = level_seed(bank, solving.volume.grid);
        let (ground, albedo) = bank.lighting.as_ref().map_or((0.0, 0.0), |lighting| {
            (lighting.ground.0.luminance(), lighting.ground.1)
        });
        let across = plan.brightest * bank.sun.y;
        Self {
            solving,
            bank,
            reader: Reader { bank },
            grid: solving.volume.grid,
            floor: bank.floor,
            factor: plan.power.map(|power| {
                if plan.reference > 0.0 {
                    power / plan.reference
                } else {
                    0.0
                }
            }),
            scaled: scaled(bank),
            survives: bank.scattering().albedo,
            albedo: albedo.clamp(0.0, 1.0),
            grounded: if across > 0.0 { ground / across } else { 0.0 },
            seed: round_seed(spreading, plan.round),
            spreading,
        }
    }

    /// Send photon `index` of the round and follow it; how many voxels' track
    /// it laid.
    fn send(&self, index: usize) -> u32 {
        let Ok(at) = u64::try_from(index) else {
            return 0;
        };
        let counts = self.solving.plan.counts;
        let mut draws = Draws::new(self.seed, at);
        let (kind, within) = if at < counts[SUN] {
            (SUN, at)
        } else {
            (SKY, at - counts[SUN])
        };
        // Each kind's places run on through the rounds, so every round adds
        // to how evenly they are spread.
        let onward = within + u64::from(self.solving.plan.round) * counts[kind];
        let place = spread(onward, self.spreading ^ SALTS[kind]);
        let way = if kind == SUN {
            -self.bank.sun
        } else {
            let up = lambertian(&mut draws);
            Vec3::new(up.x, -up.y, up.z)
        };
        self.walk(Photon::new(self.on_top(place), way, kind), &mut draws)
    }

    /// The point on the level's top face at `(u, v)` of its square.
    fn on_top(&self, (u, v): (f64, f64)) -> [f64; 3] {
        let side = real(self.grid.cells);
        [u * side, real(self.grid.layers) - 1e-9, v * side]
    }

    /// Where `at`, in voxels, lies over the Earth's curve: `(x, altitude, z)`.
    fn place(&self, at: [f64; 3]) -> (f64, f64, f64) {
        let grid = self.grid;
        (
            self.bank.centre.0 - grid.half + at[0] * grid.breadth,
            self.floor + at[1] * grid.height,
            self.bank.centre.1 - grid.half + at[2] * grid.breadth,
        )
    }

    /// The extinction of the bank's own cloud at `at`, in voxels: nought at
    /// once where its weather says none can stand.
    fn extinction(&self, at: [f64; 3]) -> f64 {
        let (x, altitude, z) = self.place(at);
        if !self.bank.may_stand((x, z), altitude) {
            return 0.0;
        }
        self.reader
            .density_at(
                point(self.bank, (x, altitude, z)),
                altitude,
                Edges::Resolved(FOOTPRINT),
            )
            .map_or(0.0, |sample| sample.extinction)
    }

    /// Follow `photon` until it leaves by the top, is lost to the ground, or
    /// has been followed as far as any is; how many voxels' track it laid.
    fn walk(&self, mut photon: Photon, draws: &mut Draws) -> u32 {
        let colour = if photon.kind == SKY { SKYLIT } else { SUNLIT };
        let mut pending = [Pending::new(colour), Pending::new(colour)];
        let (mut steps, mut laid) = (0u32, 0.0);
        while steps < MOST_STEPS {
            match self.fly(&mut photon, (&mut steps, &mut pending, &mut laid), draws) {
                Flight::Collided => {
                    // The beam's power where it first scatters.
                    if photon.beam {
                        let at = self.place(photon.at);
                        photon.weight = sunlight(self.bank, &self.solving.volume, at)
                            / self.solving.plan.brightest.max(f64::MIN_POSITIVE);
                        photon.beam = false;
                    }
                    photon.weight *= self.survives;
                    photon.way = isotropic(draws);
                    photon.scattered = true;
                }
                Flight::Escaped => break,
                Flight::Grounded => {
                    if photon.beam {
                        let (x, _, z) = self.place(photon.at);
                        photon.weight = self.grounded
                            * kept_above(self.bank, &self.solving.volume, (x, 0.0, z));
                        photon.beam = false;
                    }
                    if photon.scattered {
                        let shed = &self.solving.tally.shed[colour];
                        add(
                            shed,
                            self.column(photon.at),
                            photon.weight * self.factor[photon.kind] * FIXED,
                        );
                    }
                    if !self.reflect(&mut photon, draws) {
                        break;
                    }
                }
            }
            if photon.weight < LEAST_WEIGHT {
                if draws.next() * LEAST_WEIGHT >= photon.weight {
                    break;
                }
                photon.weight = LEAST_WEIGHT;
            }
        }
        let [voxels, fine] = &mut pending;
        voxels.flush(&self.solving.tally.voxels);
        fine.flush(&self.solving.tally.fine);
        u32::try_from(whole(laid)).unwrap_or(u32::MAX)
    }

    /// The cell of the floor a point, in voxels, stands over.
    fn column(&self, at: [f64; 3]) -> usize {
        let cells = self.grid.floor_cells();
        let cell = |along: f64| (cell_of(along).0 / FLOOR).min(cells - 1);
        cell(at[2]) * cells + cell(at[0])
    }

    /// `photon` down to the ground and back up to the floor, as the ground
    /// reflects it; whether any of it is.
    fn reflect(&self, photon: &mut Photon, draws: &mut Draws) -> bool {
        photon.weight *= self.albedo;
        if photon.weight <= 0.0 {
            return false;
        }
        let side = real(self.grid.cells);
        let fall = self.floor / (-photon.way.y).max(1e-9) / self.grid.breadth;
        let (east, north) = (
            photon.at[0] + photon.way.x * fall,
            photon.at[2] + photon.way.z * fall,
        );
        let way = lambertian(draws);
        let rise = self.floor / way.y.max(1e-9) / self.grid.breadth;
        photon.at = [
            around(east + way.x * rise, side),
            1e-9,
            around(north + way.z * rise, side),
        ];
        photon.way = way;
        photon.scattered = false;
        true
    }

    /// Fly `photon` on until it meets the cloud, leaves by the top or comes
    /// down to the floor, voxel by voxel, laying its track down as it goes:
    /// within each voxel each place it might meet the cloud drawn against the
    /// voxel's bound of the extinction, and met as often as the cloud truly
    /// there has it; `steps` counts the voxels and places it passes, and
    /// `laid` the voxels' worth of track it lays.
    fn fly(
        &self,
        photon: &mut Photon,
        (steps, pending, laid): (&mut u32, &mut [Pending; 2], &mut f64),
        draws: &mut Draws,
    ) -> Flight {
        let grid = self.grid;
        let (cells, layers) = (grid.cells, grid.layers);
        let side = real(cells);
        let extent = [grid.breadth, grid.height, grid.breadth];
        let ways = [photon.way.x, photon.way.y, photon.way.z];
        let runs = &self.solving.volume.fine.runs;
        let weight = photon.weight * self.factor[photon.kind] * FIXED / grid.breadth;
        let start = photon.at;
        // Where it stands `t` metres on, round the level's square.
        let along = |t: f64| {
            let mut at = start;
            for (axis, at) in at.iter_mut().enumerate() {
                *at += ways[axis] * t / extent[axis];
            }
            at[0] = around(at[0], side);
            at[2] = around(at[2], side);
            at
        };
        let Crossing {
            mut cell,
            mut next,
            apart,
        } = Crossing::new(grid, start, ways);
        let mut t = 0.0;
        loop {
            *steps += 1;
            if *steps >= MOST_STEPS {
                return Flight::Escaped;
            }
            let voxel = (cell[0], cell[1], cell[2]);
            let index = grid.index(voxel);
            let held = (!photon.beam)
                .then(|| runs.block(grid, voxel))
                .flatten()
                .map(|block| (block, along(t)));
            let bound = self.scaled
                * self
                    .solving
                    .bounds
                    .get(index)
                    .map_or(0.0, |&value| f64::from(value));
            let axis = nearest(next);
            let exit = next[axis];
            // A way crossing no wall, degenerate, goes nowhere.
            if !exit.is_finite() {
                return Flight::Escaped;
            }
            let mut met = None;
            if bound > 0.0 {
                let mut within = t;
                loop {
                    within -= mathf::ln((1.0 - draws.next()).max(1e-300)) / bound;
                    if within >= exit {
                        break;
                    }
                    *steps += 1;
                    let mut place = along(within);
                    place[1] = place[1].clamp(real(cell[1]), real(cell[1] + 1) - 1e-9);
                    let share = self.scaled * self.extinction(place) / bound;
                    if draws.next() < share {
                        met = Some((within, place));
                        break;
                    }
                    if *steps >= MOST_STEPS {
                        return Flight::Escaped;
                    }
                }
            }
            let end = met.map_or(exit, |(within, _)| within);
            if !photon.beam {
                self.lay(pending, (index, held), (cell, ways), (end - t, weight));
                *laid += (end - t) / grid.breadth;
            }
            if let Some((_, place)) = met {
                photon.at = place;
                return Flight::Collided;
            }
            t = exit;
            next[axis] += apart[axis];
            if axis != 1 {
                // Round the level's square, its cloud taken as running on.
                cell[axis] = if ways[axis] > 0.0 {
                    (cell[axis] + 1) % cells
                } else {
                    (cell[axis] + cells - 1) % cells
                };
            } else if ways[1] > 0.0 {
                if cell[1] + 1 >= layers {
                    return Flight::Escaped;
                }
                cell[1] += 1;
            } else if cell[1] == 0 {
                photon.at = along(t);
                photon.at[1] = 0.0;
                return Flight::Grounded;
            } else {
                cell[1] -= 1;
            }
        }
    }
}

impl Walker<'_> {
    /// Lay `length` metres of a photon's track, each metre weighing `weight`,
    /// through voxel `index`, `cell` of the level, along `ways`: where the
    /// voxel is `held` as a block, entered at a place given in voxels, its
    /// share in each fine cell it crosses, the whole track kept, the voxel's
    /// own light being the sum of its fine cells'; elsewhere in the voxel.
    fn lay(
        &self,
        [voxels, fine]: &mut [Pending; 2],
        (index, held): (usize, Option<(usize, [f64; 3])>),
        (cell, ways): ([usize; 3], [f64; 3]),
        (length, weight): (f64, f64),
    ) {
        let tally = &self.solving.tally;
        let Some((block, from)) = held else {
            voxels.lay(&tally.voxels, index, weight * length, ways);
            return;
        };
        let grid = self.grid;
        let (per, parts) = (grid.fine, real(grid.fine));
        let extent = [
            grid.breadth / parts,
            grid.height / parts,
            grid.breadth / parts,
        ];
        let side = real(grid.cells);
        let (mut at, mut next, mut apart) = ([0usize; 3], [f64::INFINITY; 3], [f64::INFINITY; 3]);
        for axis in 0..3 {
            let mut offset = from[axis] - real(cell[axis]);
            // Across the square's wrap, the photon stands a side away.
            if axis != 1 {
                offset -= side * mathf::round(offset / side);
            }
            let local = (offset * parts).clamp(0.0, parts - 1e-9);
            at[axis] = cell_of(local).0.min(per - 1);
            if ways[axis].abs() > 1e-12 {
                let wall = real(at[axis]) + if ways[axis] > 0.0 { 1.0 } else { 0.0 };
                next[axis] = (wall - local) * extent[axis] / ways[axis];
                apart[axis] = extent[axis] / ways[axis].abs();
            }
        }
        let mut t = 0.0;
        loop {
            let axis = nearest(next);
            let end = next[axis].min(length);
            let cell = block * grid.fine_cells() + fine_within(per, (at[0], at[1], at[2]));
            let onward = if ways[axis] > 0.0 {
                at[axis].checked_add(1).filter(|&step| step < per)
            } else {
                at[axis].checked_sub(1)
            };
            // What a rounding error leaves past the voxel's last wall stays in
            // its last cell.
            let Some(step) = onward.filter(|_| end < length) else {
                fine.lay(&tally.fine, cell, weight * (length - t), ways);
                return;
            };
            fine.lay(&tally.fine, cell, weight * (end - t), ways);
            t = end;
            next[axis] += apart[axis];
            at[axis] = step;
        }
    }
}

/// A photon's way through a level's voxels from where it stands: the voxel
/// it stands in, and how far along its way, in metres, the voxel's next wall
/// lies on each axis and each wall after that.
struct Crossing {
    cell: [usize; 3],
    next: [f64; 3],
    apart: [f64; 3],
}

impl Crossing {
    fn new(grid: Grid, start: [f64; 3], ways: [f64; 3]) -> Self {
        let extent = [grid.breadth, grid.height, grid.breadth];
        let mut crossing = Self {
            cell: [0; 3],
            next: [f64::INFINITY; 3],
            apart: [f64::INFINITY; 3],
        };
        for axis in 0..3 {
            let count = if axis == 1 { grid.layers } else { grid.cells };
            crossing.cell[axis] = cell_of(start[axis]).0.min(count - 1);
            if ways[axis].abs() > 1e-12 {
                let wall = real(crossing.cell[axis]) + if ways[axis] > 0.0 { 1.0 } else { 0.0 };
                crossing.next[axis] = (wall - start[axis]) * extent[axis] / ways[axis];
                crossing.apart[axis] = extent[axis] / ways[axis].abs();
            }
        }
        crossing
    }
}

/// The axis whose wall a photon reaches first, of the distances `next`.
fn nearest(next: [f64; 3]) -> usize {
    if next[0] <= next[1] && next[0] <= next[2] {
        0
    } else if next[1] <= next[2] {
        1
    } else {
        2
    }
}

/// Which field pass `pass` is of the level's: the field, whether it is a
/// fluence or an axis of the flux; and its step, `0` gathering its tally,
/// then three smoothing steps each of its smoothings.
fn field_step(pass: usize) -> (usize, usize) {
    let fluences = FLUENCES * PASSES;
    if pass < fluences {
        return (pass / PASSES, pass % PASSES);
    }
    let per = 1 + 3 * SMOOTHINGS[1];
    let (axis, step) = ((pass - fluences) / per, (pass - fluences) % per);
    (FLUENCES + axis, step)
}

/// One pass over rows `rows` of making the level's tallies its fields: its
/// tally gathered, or one of its smoothings east-west, north-south or
/// up-down, the last up-down pass packing it.
fn field_pass(
    solving: &mut Solving,
    pass: usize,
    rows: Range<usize>,
    runner: &dyn JobRunner,
) -> Option<()> {
    let grid = solving.volume.grid;
    let (cells, layers) = (grid.cells, grid.layers);
    if solving.field.is_empty() {
        solving.field = fallible::filled(grid.voxels(), 0.0)?;
        solving.smoothed = fallible::filled(grid.voxels(), 0.0)?;
    }
    let (field, step) = field_step(pass);
    let signed = field >= FLUENCES;
    let smoothings = SMOOTHINGS[usize::from(signed)];
    let range = rows.start * cells..rows.end * cells;
    if step == 0 {
        let (tally, fine) = (
            solving.tally.voxels.field(field)?,
            solving.tally.fine.field(field)?,
        );
        let (runs, per) = (&solving.volume.fine.runs, grid.fine_cells());
        let scale =
            solving.plan.reference / (FIXED * grid.breadth * grid.height * rounds(&solving.plan));
        let values = solving.field.get_mut(range)?;
        let most = band::fold(
            runner,
            values,
            (rows.start, cells),
            0.0f64,
            &|row, band| {
                let (k, j) = (row / cells, row % cells);
                let mut most = 0.0f64;
                for (i, slot) in band.iter_mut().enumerate() {
                    let held = runs.block(grid, (i, k, j)).map_or(0.0, |block| {
                        (block * per..(block + 1) * per)
                            .map(|cell| summed(fine, cell))
                            .sum()
                    });
                    let value = (summed(tally, row * cells + i) + held) * scale;
                    most = most.max(value.abs());
                    *slot = single(value);
                }
                most
            },
            &f64::max,
        );
        solving.most = if rows.start == 0 {
            most
        } else {
            solving.most.max(most)
        };
        return Some(());
    }
    // Each pass reads where the last wrote: from the gathered field into the
    // copy, and back.
    let (axis, passes) = ((step - 1) % 3, step - 1);
    let last = passes + 1 == 3 * smoothings;
    let (source, target) = if passes % 2 == 0 {
        (&solving.field, &mut solving.smoothed)
    } else {
        (&solving.smoothed, &mut solving.field)
    };
    let values = target.get_mut(range.clone())?;
    band::for_each(runner, values, (rows.start, cells), &|row, band| {
        let (k, j) = (row / cells, row % cells);
        let at = |i: usize, k: usize, j: usize| {
            f64::from(source.get(grid.index((i, k, j))).copied().unwrap_or(0.0))
        };
        for (i, slot) in band.iter_mut().enumerate() {
            let blur = |(before, after): ((usize, usize, usize), (usize, usize, usize))| {
                0.25 * at(before.0, before.1, before.2)
                    + 0.5 * at(i, k, j)
                    + 0.25 * at(after.0, after.1, after.2)
            };
            *slot = single(match axis {
                0 => blur((((i + cells - 1) % cells, k, j), ((i + 1) % cells, k, j))),
                1 => blur(((i, k, (j + cells - 1) % cells), (i, k, (j + 1) % cells))),
                _ => blur(((i, k.saturating_sub(1), j), (i, (k + 1).min(layers - 1), j))),
            });
        }
    });
    if !last {
        return Some(());
    }
    pack(solving, (field, signed), rows.clone(), passes % 2 == 0)?;
    if rows.end >= grid.rows() {
        // The tally is spent; and once every field is, the copies.
        *solving.tally.voxels.field_mut(field)? = Vec::new();
        if field + 1 >= FLUENCES + FLUXES {
            solving.field = Vec::new();
            solving.smoothed = Vec::new();
        }
    }
    Some(())
}

/// One pass over blocks `blocks` of making the level's fine tallies its fine
/// fields, as [`field_pass`] makes its voxels' own: a neighbour its runs do
/// not hold stands in as the cell itself, so the blur neither spills light
/// out of the cloud nor dims its edge.
fn fine_pass(
    solving: &mut Solving,
    pass: usize,
    blocks: Range<usize>,
    runner: &dyn JobRunner,
) -> Option<()> {
    let grid = solving.volume.grid;
    let per = grid.fine_cells();
    let held = solving.volume.fine.runs.blocks * per;
    if solving.field.len() != held {
        solving.field = fallible::filled(held, 0.0)?;
        solving.smoothed = fallible::filled(held, 0.0)?;
    }
    let (field, step) = field_step(pass);
    let signed = field >= FLUENCES;
    let smoothings = SMOOTHINGS[usize::from(signed)];
    let range = blocks.start * per..blocks.end * per;
    let band = (0, per * BAND_BLOCKS);
    if step == 0 {
        let tally = solving.tally.fine.field(field)?;
        // A fine cell holds its voxel's share of the volume.
        let scale = solving.plan.reference * real(per)
            / (FIXED * grid.breadth * grid.height * rounds(&solving.plan));
        let first = range.start;
        let values = solving.field.get_mut(range)?;
        let most = band::fold(
            runner,
            values,
            band,
            0.0f64,
            &|number, values| {
                let mut most = 0.0f64;
                let from = first + number * per * BAND_BLOCKS;
                for (offset, slot) in values.iter_mut().enumerate() {
                    let value = summed(tally, from + offset) * scale;
                    most = most.max(value.abs());
                    *slot = single(value);
                }
                most
            },
            &f64::max,
        );
        solving.most = if blocks.start == 0 {
            most
        } else {
            solving.most.max(most)
        };
        return Some(());
    }
    let (axis, passes) = ((step - 1) % 3, step - 1);
    let last = passes + 1 == 3 * smoothings;
    let (source, target) = if passes % 2 == 0 {
        (&solving.field, &mut solving.smoothed)
    } else {
        (&solving.smoothed, &mut solving.field)
    };
    let (runs, voxels) = (&solving.volume.fine.runs, &solving.voxels);
    let (along, up) = (grid.cells * grid.fine, grid.layers * grid.fine);
    let first = blocks.start;
    let values = target.get_mut(range.clone())?;
    band::for_each(runner, values, band, &|number, values| {
        let from = first + number * BAND_BLOCKS;
        for (offset, slot) in values.iter_mut().enumerate() {
            let (block, within) = (from + offset / per, offset % per);
            let read = |index: usize| f64::from(source.get(index).copied().unwrap_or(0.0));
            let here = read(block * per + within);
            let (i, k, j) = fine_cell(grid, voxels.get(block).copied().unwrap_or(0), within);
            let beside = |cell: (usize, usize, usize)| runs.fine(grid, cell).map_or(here, read);
            let (before, after) = match axis {
                0 => (((i + along - 1) % along, k, j), ((i + 1) % along, k, j)),
                1 => ((i, k, (j + along - 1) % along), (i, k, (j + 1) % along)),
                _ => ((i, k.saturating_sub(1), j), (i, (k + 1).min(up - 1), j)),
            };
            *slot = single(0.25 * beside(before) + 0.5 * here + 0.25 * beside(after));
        }
    });
    if !last {
        return Some(());
    }
    pack_fine(solving, (field, signed), range, passes % 2 == 0)?;
    if blocks.end >= solving.volume.fine.runs.blocks {
        *solving.tally.fine.field_mut(field)? = Vec::new();
        if field + 1 >= FLUENCES + FLUXES {
            solving.field = Vec::new();
            solving.smoothed = Vec::new();
            solving.voxels = Vec::new();
        }
    }
    Some(())
}

/// Fine cells `range` of fine field `field` of the level's, signed or not,
/// packed from the copy it was last smoothed into, or else the field itself;
/// the field reserved whole at its first cell.
fn pack_fine(
    solving: &mut Solving,
    (field, signed): (usize, bool),
    range: Range<usize>,
    copy: bool,
) -> Option<()> {
    if range.start == 0 {
        let held = solving.volume.fine.runs.blocks * solving.volume.grid.fine_cells();
        let packed = Packed::reserved(held, Packed::scale_for(solving.most, signed), signed)?;
        *fine_field(&mut solving.volume.fine, field)? = packed;
    }
    let source = if copy {
        &solving.smoothed
    } else {
        &solving.field
    };
    let packed = fine_field(&mut solving.volume.fine, field)?;
    let packing = Packed {
        scale: packed.scale,
        codes: Vec::new(),
        signed,
    };
    for (code, &value) in packed
        .codes
        .get_mut(range.clone())?
        .iter_mut()
        .zip(source.get(range)?)
    {
        *code = packing.code(f64::from(value));
    }
    Some(())
}

/// Field `field` of the fine light: a fluence, then each axis of the flux.
fn fine_field(fine: &mut Fine, field: usize) -> Option<&mut Packed> {
    match field.checked_sub(FLUENCES) {
        None => fine.fluence.get_mut(field),
        Some(axis) => fine.flux.get_mut(axis),
    }
}

/// Rows `rows` of field `field` of the level's, signed or not, packed from
/// the copy it was last smoothed into, or else the field itself.
fn pack(
    solving: &mut Solving,
    (field, signed): (usize, bool),
    rows: Range<usize>,
    copy: bool,
) -> Option<()> {
    let grid = solving.volume.grid;
    let range = rows.start * grid.cells..rows.end * grid.cells;
    if rows.start == 0 {
        let packed = Packed::reserved(
            grid.voxels(),
            Packed::scale_for(solving.most, signed),
            signed,
        )?;
        if signed {
            *solving.volume.flux.get_mut(field - FLUENCES)? = packed;
        } else {
            *solving.volume.fluence.get_mut(field)? = packed;
        }
    }
    let source = if copy {
        &solving.smoothed
    } else {
        &solving.field
    };
    let packed = if signed {
        solving.volume.flux.get_mut(field - FLUENCES)?
    } else {
        solving.volume.fluence.get_mut(field)?
    };
    let packing = Packed {
        scale: packed.scale,
        codes: Vec::new(),
        signed,
    };
    for (code, &value) in packed
        .codes
        .get_mut(range.clone())?
        .iter_mut()
        .zip(source.get(range)?)
    {
        *code = packing.code(f64::from(value));
    }
    Some(())
}

/// Rows `rows` of the level's optical depth toward the sun packed.
fn map_rows(solving: &mut Solving, rows: Range<usize>, runner: &dyn JobRunner) -> Option<()> {
    let grid = solving.volume.grid;
    let cells = grid.cells;
    if rows.start == 0 {
        solving.volume.depth = Packed::reserved(grid.voxels(), DEPTH_SCALE, false)?;
    }
    let packing = Packed {
        scale: DEPTH_SCALE,
        codes: Vec::new(),
        signed: false,
    };
    let source = &solving.depth;
    let codes = solving
        .volume
        .depth
        .codes
        .get_mut(rows.start * cells..rows.end * cells)?;
    band::for_each(runner, codes, (rows.start, cells), &|row, band| {
        for (i, slot) in band.iter_mut().enumerate() {
            *slot = packing.code(f64::from(
                source.get(row * cells + i).copied().unwrap_or(0.0),
            ));
        }
    });
    Some(())
}

/// How much of the sky overhead each cell of the level's floor leaves open:
/// its columns' vertical transmittance, on average.
fn open_floor(solving: &Solving, cells: usize) -> Option<Vec<f32>> {
    let grid = solving.volume.grid;
    let (columns, floor) = (cells * cells, grid.floor_cells());
    let transmitted = |column: usize| {
        let depth: f64 = (0..grid.layers)
            .filter_map(|k| solving.sigma.get(k * columns + column))
            .map(|&value| f64::from(value))
            .sum::<f64>()
            * grid.height;
        mathf::exp(-depth)
    };
    fallible::collected(
        floor * floor,
        (0..floor * floor).map(|cell| {
            let (ci, cj) = (cell % floor, cell / floor);
            let mut sum = 0.0;
            for j in cj * FLOOR..(cj * FLOOR + FLOOR).min(cells) {
                for i in ci * FLOOR..(ci * FLOOR + FLOOR).min(cells) {
                    sum += transmitted(j * cells + i);
                }
            }
            single(sum / real(FLOOR * FLOOR))
        }),
    )
}

/// The irradiance of each colour the level's cloud sheds down from its floor
/// over each cell of it.
fn shed_floor(solving: &Solving) -> Option<[Vec<f32>; 2]> {
    let grid = solving.volume.grid;
    let floor = grid.floor_cells();
    let size = 2.0 * grid.half / real(floor);
    let scale = solving.plan.reference / (FIXED * size * size * rounds(&solving.plan));
    let shed = |colour: usize| {
        let tally = &solving.tally.shed[colour];
        fallible::collected(
            floor * floor,
            (0..floor * floor).map(|cell| {
                if tally.is_empty() {
                    0.0
                } else {
                    single(summed(tally, cell) * scale)
                }
            }),
        )
    };
    Some([shed(SUNLIT)?, shed(SKYLIT)?])
}

/// `map`, a square `cells` a side, blurred a quarter, a half and a quarter
/// each way round its square, as the photons ran round it.
fn smoothed(map: &[f32], cells: usize) -> Option<Vec<f32>> {
    let at =
        |map: &[f32], i: usize, j: usize| f64::from(map.get(j * cells + i).copied().unwrap_or(0.0));
    let (before, after) = (
        |i: usize| (i + cells - 1) % cells,
        |i: usize| (i + 1) % cells,
    );
    let east = fallible::collected(
        map.len(),
        (0..map.len()).map(|index| {
            let (i, j) = (index % cells, index / cells);
            single(0.25 * at(map, before(i), j) + 0.5 * at(map, i, j) + 0.25 * at(map, after(i), j))
        }),
    )?;
    fallible::collected(
        east.len(),
        (0..east.len()).map(|index| {
            let (i, j) = (index % cells, index / cells);
            single(
                0.25 * at(&east, i, before(j))
                    + 0.5 * at(&east, i, j)
                    + 0.25 * at(&east, i, after(j)),
            )
        }),
    )
}

#[cfg(test)]
#[path = "light_tests.rs"]
mod tests;
