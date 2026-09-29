//! The ribbon of light behind the minimal clock.
//!
//! Five soft strands run from the left edge to the right, each one Bézier
//! curve across the width. The whole ribbon roams along a course travelling
//! waves carry, and each strand wanders about its own lane in it on waves of
//! its own, so the strands cross, gather and fan apart while every path stays
//! one smooth curve. A bright point runs along each strand, each hangs a
//! curtain that fades to black beneath it, and the ribbon's upper edge glows
//! into the dark above. The exposures add, and the sum is toned through an
//! ember's heat from deep red to near-white gold.
//!
//! Wherever a strand's light would reach the clock's text, the strand is pushed
//! down a Bézier bump of its own control points, just far enough to hold the
//! text's clear space dark, so a path passing beneath the text is still one
//! curve.
//!
//! The light is summed at every other pixel each way and blended back up to
//! every pixel as it is toned and dithered, and a frame repaints only the rows
//! it reaches in each column and those it reached the frame before.

use alloc::vec::Vec;
use core::f64::consts::{FRAC_PI_2, TAU};
use core::iter::successors;
use core::ops::Range;

use tairix_raster::{DitherRow, Pixel, Surface};
use tairix_util::{fallible, mathf};
use tairix_wm::{Rect, Region};

/// The degree of every strand's path.
const DEGREE: u32 = 7;

/// A path's control points, evenly spaced across it.
const CONTROLS: usize = DEGREE as usize + 1;

/// How far past each edge of the screen a path runs on, in screen widths: a
/// Bézier curve bends most readily near its ends, so they lie off the screen.
const OVERHANG: f64 = 0.15;

/// Where each control point stands across the width, in screen widths.
const ABSCISSAE: [f64; CONTROLS] = {
    let mut across = [0.0; CONTROLS];
    let mut point = 0;
    while point <= DEGREE {
        across[point as usize] = -OVERHANG + point as f64 * (1.0 + 2.0 * OVERHANG) / DEGREE as f64;
        point += 1;
    }
    across
};

/// A travelling wave across the width:
/// `amplitude · sin(τ·(u / wavelength − t / period) + phase)`, for `u` across
/// the width and `t` in seconds. A positive period travels rightward.
#[derive(Copy, Clone, Debug)]
struct Wave {
    amplitude: f64,
    /// In screen widths.
    wavelength: f64,
    /// In seconds.
    period: f64,
    phase: f64,
}

impl Wave {
    /// The wave at `u` across the width, `t` seconds on.
    fn at(self, u: f64, t: f64) -> f64 {
        self.amplitude * mathf::sin(TAU * (u / self.wavelength - t / self.period) + self.phase)
    }
}

/// Where between the highest and the lowest it may run the ribbon's course
/// stands at each control point, as a share of half that range either side of
/// its middle. The amplitudes sum to one, so the course keeps to its range.
const COURSE: [Wave; 3] = [
    Wave {
        amplitude: 0.55,
        wavelength: 2.0,
        period: 71.0,
        phase: FRAC_PI_2,
    },
    Wave {
        amplitude: 0.25,
        wavelength: 1.3,
        period: -47.0,
        phase: 2.2,
    },
    Wave {
        amplitude: 0.2,
        wavelength: 0.8,
        period: 29.0,
        phase: 4.4,
    },
];

/// How far inside the top and the bottom of the screen every path stays, in
/// screen heights.
const EDGE_MARGIN: f64 = 0.03;

/// How far beyond the clock's text the ribbon's control points are held wholly
/// beneath it, and how far further they are let go over, in screen heights: a
/// path climbing out from beneath the text rises no more steeply on a narrow
/// screen than on a wide one.
const SHADOW_HOLD: f64 = 0.09;
const SHADOW_FADE: f64 = 0.36;

/// The share of their lanes' spacing and of their wandering the strands give
/// up beneath the clock's text, where the ribbon has least room.
const GATHER: f64 = 0.25;

/// The least a column beneath the text moves clear of it per pixel of push,
/// however its paths steepen as they are pushed.
const LEAST_GIVE: f64 = 0.25;

/// How gently a push eases in as the ribbon nears the text, in screen heights.
const PUSH_EASE: f64 = 0.02;

/// How far below the text's clearance a push holds the light, in pixels, so
/// rounding cannot carry it back across.
const CLEAR_SLACK: f64 = 1.0 / 16.0;

/// The share of each strand's light and curtain that holds still: the bright
/// point running along it carries the rest, and its curtain a share of that.
const QUIET_LIGHT: f64 = 0.28;
const CURTAIN_FLARE_SHARE: f64 = 0.18;

/// Stations along the width, `(across, value)`, that a smooth curve passes
/// through: the first at the left edge, the last at the right.
type Stations = &'static [(f64, f64)];

/// One strand of the ribbon.
struct Strand {
    /// How far beneath the ribbon's course it runs on average, in screen
    /// heights.
    lane: f64,
    /// How it wanders about its lane, in screen heights: waves of its own, so
    /// no two strands move alike.
    wander: [Wave; 2],
    /// The radius of its glow, in screen heights.
    glow: f64,
    /// How bright it is along the width, in exposure, before the quiet share
    /// is taken of it.
    light: Stations,
    /// The exposure the bright point running along it adds at its peak, and
    /// how it runs: one wavelength across the screen, so one point is always
    /// on it, whole or wrapping through an edge.
    flare: f64,
    sweep: Wave,
    /// The curtain it hangs beneath itself, in exposure.
    curtain: Stations,
    /// How far its curtain falls, in screen heights.
    drape: Stations,
}

/// The ribbon's strands, the brightest running highest.
const STRANDS: [Strand; 5] = [
    Strand {
        lane: 0.0,
        wander: [
            Wave {
                amplitude: 0.060,
                wavelength: 1.35,
                period: 31.0,
                phase: 0.9,
            },
            Wave {
                amplitude: 0.040,
                wavelength: 0.8,
                period: -23.0,
                phase: 4.1,
            },
        ],
        glow: 0.020,
        light: &[
            (0.0, 0.045),
            (0.1, 0.08),
            (0.2, 0.22),
            (0.3, 0.42),
            (0.4, 0.65),
            (0.45, 0.9),
            (0.5, 1.12),
            (0.6, 1.3),
            (0.75, 1.35),
            (0.84, 1.22),
            (0.9, 0.65),
            (0.95, 0.47),
            (1.0, 0.36),
        ],
        flare: 0.90,
        sweep: Wave {
            amplitude: 1.0,
            wavelength: 1.0,
            period: 13.0,
            phase: 0.2,
        },
        curtain: &[
            (0.0, 0.05),
            (0.15, 0.1),
            (0.3, 0.13),
            (0.45, 0.15),
            (0.6, 0.22),
            (0.75, 0.3),
            (0.85, 0.2),
            (0.95, 0.12),
            (1.0, 0.1),
        ],
        drape: &[(0.0, 0.022), (0.5, 0.016), (0.75, 0.018), (1.0, 0.024)],
    },
    Strand {
        lane: 0.03,
        wander: [
            Wave {
                amplitude: 0.058,
                wavelength: 1.1,
                period: -27.0,
                phase: 2.3,
            },
            Wave {
                amplitude: 0.042,
                wavelength: 0.7,
                period: 19.0,
                phase: 0.4,
            },
        ],
        glow: 0.0185,
        light: &[
            (0.0, 0.07),
            (0.2, 0.09),
            (0.33, 0.12),
            (0.5, 0.1),
            (0.62, 0.12),
            (0.75, 0.28),
            (0.84, 0.56),
            (0.9, 0.85),
            (0.95, 0.73),
            (1.0, 0.56),
        ],
        flare: 0.54,
        sweep: Wave {
            amplitude: 1.0,
            wavelength: 1.0,
            period: -17.0,
            phase: 1.3,
        },
        curtain: &[
            (0.0, 0.03),
            (0.3, 0.04),
            (0.5, 0.03),
            (0.65, 0.08),
            (0.75, 0.2),
            (0.85, 0.25),
            (0.95, 0.24),
            (1.0, 0.22),
        ],
        drape: &[(0.0, 0.016), (0.5, 0.018), (0.9, 0.022), (1.0, 0.024)],
    },
    Strand {
        lane: 0.06,
        wander: [
            Wave {
                amplitude: 0.062,
                wavelength: 1.6,
                period: 35.0,
                phase: 5.2,
            },
            Wave {
                amplitude: 0.038,
                wavelength: 0.9,
                period: -17.0,
                phase: 2.8,
            },
        ],
        glow: 0.015,
        light: &[
            (0.0, 0.1),
            (0.2, 0.12),
            (0.35, 0.16),
            (0.5, 0.12),
            (0.6, 0.2),
            (0.75, 0.28),
            (0.85, 0.25),
            (1.0, 0.17),
        ],
        flare: 0.24,
        sweep: Wave {
            amplitude: 1.0,
            wavelength: 1.0,
            period: 19.0,
            phase: 2.6,
        },
        curtain: &[
            (0.0, 0.03),
            (0.3, 0.03),
            (0.5, 0.04),
            (0.65, 0.09),
            (0.75, 0.12),
            (0.9, 0.1),
            (1.0, 0.08),
        ],
        drape: &[(0.0, 0.016), (0.75, 0.02), (1.0, 0.022)],
    },
    Strand {
        lane: 0.09,
        wander: [
            Wave {
                amplitude: 0.057,
                wavelength: 1.25,
                period: -41.0,
                phase: 3.6,
            },
            Wave {
                amplitude: 0.043,
                wavelength: 0.75,
                period: 29.0,
                phase: 1.7,
            },
        ],
        glow: 0.014,
        light: &[
            (0.0, 0.1),
            (0.2, 0.12),
            (0.35, 0.15),
            (0.5, 0.09),
            (0.66, 0.09),
            (0.8, 0.1),
            (1.0, 0.08),
        ],
        flare: 0.16,
        sweep: Wave {
            amplitude: 1.0,
            wavelength: 1.0,
            period: -23.0,
            phase: 4.0,
        },
        curtain: &[
            (0.0, 0.015),
            (0.5, 0.015),
            (0.75, 0.03),
            (0.9, 0.03),
            (1.0, 0.02),
        ],
        drape: &[(0.0, 0.014), (0.6, 0.018), (1.0, 0.02)],
    },
    Strand {
        lane: 0.12,
        wander: [
            Wave {
                amplitude: 0.061,
                wavelength: 1.45,
                period: 25.0,
                phase: 0.2,
            },
            Wave {
                amplitude: 0.039,
                wavelength: 0.85,
                period: -33.0,
                phase: 5.9,
            },
        ],
        glow: 0.015,
        light: &[
            (0.0, 0.06),
            (0.2, 0.12),
            (0.35, 0.16),
            (0.5, 0.08),
            (0.7, 0.07),
            (0.85, 0.08),
            (1.0, 0.06),
        ],
        flare: 0.20,
        sweep: Wave {
            amplitude: 1.0,
            wavelength: 1.0,
            period: 29.0,
            phase: 5.1,
        },
        curtain: &[
            (0.0, 0.01),
            (0.2, 0.03),
            (0.35, 0.03),
            (0.5, 0.012),
            (1.0, 0.01),
        ],
        drape: &[(0.0, 0.014), (1.0, 0.018)],
    },
];

/// Strands in the ribbon.
const STRAND_COUNT: usize = STRANDS.len();

/// The glow into the dark above the ribbon's upper edge, in exposure: broad
/// where the ribbon sweeps up the sides, soft across the middle beneath the
/// clock.
const HALO: Stations = &[
    (0.0, 0.2),
    (0.1, 0.22),
    (0.2, 0.14),
    (0.35, 0.08),
    (0.5, 0.07),
    (0.65, 0.08),
    (0.8, 0.14),
    (0.9, 0.26),
    (1.0, 0.22),
];

/// How far the glow above falls off, in screen heights.
const HALO_FALL: Stations = &[
    (0.0, 0.026),
    (0.1, 0.026),
    (0.2, 0.018),
    (0.35, 0.012),
    (0.5, 0.011),
    (0.65, 0.012),
    (0.8, 0.018),
    (0.9, 0.028),
    (1.0, 0.03),
];

/// How far the glow above falls off into the ribbon itself, in screen heights.
const INWARD_FALL: f64 = 0.006;

/// How far apart two paths blend into the ribbon's one upper edge, in screen
/// heights, so the edge turns smoothly where they cross.
const TOP_BLEND: f64 = 0.01;

/// A strand's bright core, as a share of its glow's radius, and the share of
/// its light the glow carries.
const CORE: f64 = 0.4;
const GLOW_SHARE: f64 = 0.48;

/// The least a curtain eases in over above its strand, in pixels.
const LEAST_EASE: f32 = 0.5;

/// How far a strand is ever drawn from its path, in glow radii: the length
/// of the profile table.
const STRAND_RADII: u16 = 3;

/// Steps of the strand profile table per glow radius.
const PROFILE_STEPS: u16 = 128;
const PROFILE_LEN: usize = STRAND_RADII as usize * PROFILE_STEPS as usize;

/// Light that does not lift a pixel off black: the toning table's first step.
const LEAST_LIGHT: f64 = 1.0 / FINE as f64;

/// The faintest any one term is drawn to: an eighth of the least light, so no
/// tail left undrawn moves a pixel by more than an eighth of a toning step.
const TERM_CUT: f64 = LEAST_LIGHT / 8.0;

/// The faintest a glow is drawn to outside the ribbon, where nothing else's
/// light reaches it.
const OUTER_CUT: f64 = LEAST_LIGHT / 2.0;

/// The toning table: `FINE` steps a unit of exposure up to the `KNEE`, where
/// nearly all the light lies and the eye is most sensitive, then `COARSE`
/// ones up to the `BRIGHTEST` light it tones.
const FINE: u16 = 1024;
const COARSE: u16 = 64;
const KNEE: u16 = 2;
const BRIGHTEST: u16 = 10;
const TONE_LEN: usize =
    KNEE as usize * FINE as usize + (BRIGHTEST - KNEE) as usize * COARSE as usize;

/// The ember's colour by the luma it shows: sRGB channels read off the
/// storyboard, from the faintest red to the hottest gold.
const EMBER: [(f64, [f64; 3]); 19] = [
    (0.0, [0.0, 0.0, 0.0]),
    (12.0, [22.0, 6.0, 0.0]),
    (24.0, [50.0, 12.0, 0.0]),
    (42.0, [100.0, 21.0, 0.0]),
    (56.0, [131.0, 28.0, 0.0]),
    (71.0, [163.0, 36.0, 0.0]),
    (91.0, [196.0, 52.0, 0.0]),
    (105.0, [220.0, 66.0, 1.0]),
    (119.0, [243.0, 80.0, 2.0]),
    (131.0, [252.0, 95.0, 2.0]),
    (143.0, [253.0, 115.0, 3.0]),
    (155.0, [254.0, 134.0, 5.0]),
    (168.0, [254.0, 155.0, 11.0]),
    (181.0, [254.0, 174.0, 23.0]),
    (189.0, [254.0, 185.0, 38.0]),
    (203.0, [254.0, 205.0, 65.0]),
    (219.0, [254.0, 225.0, 104.0]),
    (236.0, [254.0, 245.0, 160.0]),
    (244.0, [254.0, 253.0, 190.0]),
];

/// The luma an unbounded exposure approaches: the last station's.
const EMBER_TOP: f64 = EMBER[EMBER.len() - 1].0;

/// A one in each channel's `u16` lane: a value multiplied by it lands in all
/// three, so one add dithers every channel, and it marks the low bit halving
/// must not carry into the lane beneath.
const LANE_ONES: u64 = 0x0000_0001_0001_0001;

/// Pixels between neighbouring samples of the light, each way. Every term is
/// soft enough to span several samples, so the light is summed at a quarter
/// of the pixels and blended back up to all of them.
const PIXELS_PER_SAMPLE: u32 = 2;

/// Sample columns summed together before their pixels are written, so the
/// light is summed down a column and written along a row.
const CHUNK: u32 = 16;

/// Lanes in a row of a chunk's toned samples: its columns and the right
/// neighbour they blend into.
const CHUNK_LANES: usize = CHUNK as usize + 1;

/// Sample columns whose pixels are repainted as one strip.
const STRIP: u32 = 32;

/// What a sample column holds still: where it stands among a path's control
/// points, its light, and how far each term of it reaches.
#[derive(Copy, Clone, Debug, Default)]
struct Column {
    /// The weight each control point of a path takes here.
    basis: [f32; CONTROLS],
    light: [f32; STRAND_COUNT],
    /// How far each strand is drawn from its path, in glow radii.
    reach: [f32; STRAND_COUNT],
    curtain: [f32; STRAND_COUNT],
    drape: [Fall; STRAND_COUNT],
    halo: f32,
    /// The glow above falling off upward, and into the ribbon.
    halo_fall: (Fall, Fall),
}

/// A falloff's length and how far it is drawn, in pixels, and the decay one
/// sample applies.
#[derive(Copy, Clone, Debug, Default)]
struct Fall {
    length: f32,
    reach: f32,
    step: f32,
}

impl Fall {
    /// A falloff over `length` pixels of a light as bright as `light`, drawn
    /// until it falls below `cut`.
    fn new(length: f64, light: f64, cut: f64) -> Self {
        let length = mathf::fmax(length, 1.0);
        Self {
            length: narrow(length),
            reach: narrow(length * falloff_reach(light, cut)),
            step: narrow(mathf::exp(-f64::from(PIXELS_PER_SAMPLE) / length)),
        }
    }
}

/// Where a sample column's light falls this frame.
#[derive(Copy, Clone, Debug, Default)]
struct Placed {
    /// Each strand's path, in pixels.
    at: [f32; STRAND_COUNT],
    /// Each strand's exposure and the curtain beneath it this frame.
    light: [f32; STRAND_COUNT],
    curtain: [f32; STRAND_COUNT],
    /// Each strand's glow radius along the vertical, widened by its slope.
    glow: [f32; STRAND_COUNT],
    /// The ribbon's upper edge, which its halo glows above.
    top: f32,
    /// The pixel rows the light reaches, blending included.
    lit: (u32, u32),
}

/// What every column shares: the strand cross-section by distance from the
/// path, in glow radii, and exposure to dithering lanes of 8.8 fixed-point
/// channels.
struct Shape {
    profile: Vec<f32>,
    tone: Vec<u64>,
}

/// How far the ribbon's course may roam at each control point, in pixels,
/// the share of their lanes and wandering the strands keep there, and how the
/// ribbon keeps clear of the clock's text.
#[derive(Debug, Default)]
struct Roam {
    high: [f64; CONTROLS],
    low: [f64; CONTROLS],
    gather: [f64; CONTROLS],
    clearance: Option<Clearance>,
}

/// How the clock's text keeps the ribbon's light out of its clear space.
#[derive(Clone, Debug)]
struct Clearance {
    /// The highest any light may reach over the text's columns, in pixels:
    /// beneath the clear space by the rows a sample blends into above its own.
    below: f64,
    /// The sample columns whose pixels meet the clear space.
    columns: Range<usize>,
    /// How far a push moves each control point per pixel of push: wholly
    /// beneath the text, falling away to nothing either side of it.
    shadow: [f64; CONTROLS],
}

/// How a sample column answers a push down the text's shadow: how far its
/// paths move, and how their slopes turn, per pixel of push.
#[derive(Copy, Clone, Debug, Default)]
struct Yield {
    lift: f64,
    tilt: f64,
}

/// The ribbon: its sample columns, how it roams, its paths and where their
/// light falls this frame and fell the last, the strips a frame repaints, and
/// the scratch light is summed and toned in.
pub(super) struct Light {
    /// The screen, in pixels.
    size: (u32, u32),
    /// The clock text's clear space.
    exclusion: Rect,
    /// The animation time `paths` stand at.
    time: f64,
    /// The samples, one for every two pixels each way.
    grid: (u32, u32),
    columns: Vec<Column>,
    roam: Roam,
    /// Each sample column's answer to a push; those beneath the text are the
    /// ones read.
    yields: Vec<Yield>,
    /// Each strand's path this frame: its control points, in pixels.
    paths: [[f64; CONTROLS]; STRAND_COUNT],
    placed: Vec<Placed>,
    drawn: Vec<(u32, u32)>,
    strips: Vec<Rect>,
    shape: Shape,
    /// One sample column's light, summed down it.
    sums: Vec<f32>,
    /// A chunk's samples toned, row by row, a chunk and its right neighbour
    /// wide.
    toned: Vec<u64>,
    /// A pixel row blended from the two sample rows about it.
    between: Vec<u64>,
}

impl Light {
    /// The ribbon for a `size` screen, placed around `exclusion` as it stands
    /// at `t` seconds, or `None` when the screen is empty or the heap will not
    /// give it.
    pub(super) fn new(size: (u32, u32), exclusion: Rect, t: f64) -> Option<Self> {
        let (width, height) = size;
        if width == 0 || height == 0 {
            return None;
        }
        let grid = (
            width.div_ceil(PIXELS_PER_SAMPLE),
            height.div_ceil(PIXELS_PER_SAMPLE),
        );
        let count = index_of(grid.0);
        let depth = index_of(grid.1);
        let mut columns = Vec::new();
        let mut yields = Vec::new();
        let mut placed = Vec::new();
        let mut drawn = Vec::new();
        let mut strips = Vec::new();
        let mut sums = Vec::new();
        let mut toned = Vec::new();
        let mut between = Vec::new();
        let mut tone = Vec::new();
        let mut profile = Vec::new();
        if !(fallible::reserve(&mut columns, count)
            && fallible::grow_to(&mut yields, count, Yield::default())
            && fallible::grow_to(&mut placed, count, Placed::default())
            && fallible::grow_to(&mut drawn, count, (0, 0))
            && fallible::reserve(&mut strips, index_of(grid.0.div_ceil(STRIP)))
            && fallible::grow_to(&mut sums, depth, 0.0)
            && fallible::grow_to(&mut toned, CHUNK_LANES.checked_mul(depth)?, 0)
            && fallible::grow_to(&mut between, CHUNK_LANES, 0)
            && fallible::reserve(&mut tone, TONE_LEN)
            && fallible::reserve(&mut profile, PROFILE_LEN))
        {
            return None;
        }
        let tall = f64::from(height);
        columns.extend((0..grid.0).map(|column| column_at(sample_across(column, width), tall)));
        tone.extend((0..TONE_LEN).map(tone_entry));
        profile.extend((0..PROFILE_LEN).map(profile_at));
        let mut light = Self {
            size,
            exclusion: Rect::EMPTY,
            time: t,
            grid,
            columns,
            roam: Roam::default(),
            yields,
            paths: [[0.0; CONTROLS]; STRAND_COUNT],
            placed,
            drawn,
            strips,
            shape: Shape { profile, tone },
            sums,
            toned,
            between,
        };
        light.arrange(on_screen(exclusion, size));
        light.place(t);
        light.settle();
        Some(light)
    }

    /// Move the ribbon to `t` seconds around `exclusion`, adding to `damage`
    /// every pixel whose light may have changed and answering whether it
    /// moved. The strips [`paint_moved`](Self::paint_moved) repaints cover
    /// both where the light falls and where it fell.
    pub(super) fn step(&mut self, t: f64, exclusion: Rect, damage: &mut Region) -> bool {
        let exclusion = on_screen(exclusion, self.size);
        // The very same instant around the very same text is the frame already
        // placed.
        if t.to_bits() == self.time.to_bits() && exclusion == self.exclusion {
            self.strips.clear();
            return false;
        }
        self.time = t;
        if exclusion != self.exclusion {
            self.arrange(exclusion);
        }
        self.place(t);
        self.strips.clear();
        let (width, height) = self.size;
        let mut start = 0;
        while start < self.grid.0 {
            let end = start.saturating_add(STRIP).min(self.grid.0);
            let mut rows = (u32::MAX, 0);
            for index in index_range(start..end) {
                for lit in [self.drawn[index], self.placed[index].lit] {
                    if lit.0 < lit.1 {
                        rows = (rows.0.min(lit.0), rows.1.max(lit.1));
                    }
                }
            }
            if rows.0 < rows.1 {
                // The pixel before the strip's first sample blends it, and the
                // one on the next strip's first sample is that strip's alone.
                let left = pixel_of(start).saturating_sub(1);
                let right = pixel_of(end).min(width);
                let strip = rect(left..right, rows.0..rows.1.min(height));
                damage.add(strip);
                self.strips.push(strip);
            }
            start = end;
        }
        self.settle();
        true
    }

    /// Repaint the strips the last [`step`](Self::step) moved, handing each
    /// to `after` once it is painted.
    pub(super) fn paint_moved(
        &mut self,
        surface: &mut Surface,
        mut after: impl FnMut(&mut Surface, Rect),
    ) {
        for index in 0..self.strips.len() {
            let strip = self.strips[index];
            self.paint(surface, strip);
            after(surface, strip);
        }
    }

    /// Paint the ribbon as it stands over `area` of `surface`, black wherever
    /// its light does not reach.
    ///
    /// Every sample is summed the same way whatever area asks for it, so a
    /// part repainted matches the whole painted at once pixel for pixel.
    pub(super) fn paint(&mut self, surface: &mut Surface, area: Rect) {
        let (width, height) = self.size;
        let area = on_screen(area, self.size);
        let (Ok(left), Ok(top)) = (u32::try_from(area.left()), u32::try_from(area.top())) else {
            return;
        };
        let pixels = (
            left..left.saturating_add(area.width).min(width),
            top..top.saturating_add(area.height).min(height),
        );
        if pixels.0.is_empty() || pixels.1.is_empty() {
            return;
        }
        // The samples those pixels blend: the one at or before each pixel,
        // and the one after.
        let samples = (
            pixels.0.start / PIXELS_PER_SAMPLE..pixels.0.end.div_ceil(PIXELS_PER_SAMPLE),
            pixels.1.start / PIXELS_PER_SAMPLE
                ..(pixels.1.end / PIXELS_PER_SAMPLE + 1).min(self.grid.1),
        );
        let depth = index_of(samples.1.end - samples.1.start);
        let mut first = samples.0.start;
        let mut carried = false;
        while first < samples.0.end {
            let end = first.saturating_add(CHUNK).min(samples.0.end);
            for lane in u32::from(carried)..=(end - first) {
                let column = (first + lane).min(self.grid.0 - 1);
                self.tone_column(column, samples.1.clone(), index_of(lane));
            }
            let owned = pixel_of(first).max(pixels.0.start)..pixel_of(end).min(pixels.0.end);
            let used = index_of(end - first) + 1;
            self.write(
                surface,
                (first, owned),
                (samples.1.start, depth, pixels.1.clone()),
                used,
            );
            // The neighbour this chunk blended into is the next chunk's first
            // column: keep it rather than sum it again.
            for row in self
                .toned
                .as_chunks_mut::<CHUNK_LANES>()
                .0
                .iter_mut()
                .take(depth)
            {
                row[0] = row[used - 1];
            }
            carried = true;
            first = end;
        }
    }

    /// Sum sample `column`'s light over sample `rows` and tone it into `lane`
    /// of the chunk.
    fn tone_column(&mut self, column: u32, rows: Range<u32>, lane: usize) {
        let depth = index_of(rows.end - rows.start);
        let Self {
            columns,
            placed,
            shape,
            sums,
            toned,
            ..
        } = self;
        let Some(sums) = sums.get_mut(..depth) else {
            return;
        };
        sums.fill(0.0);
        let index = index_of(column);
        gather(&columns[index], &placed[index], shape, rows, sums);
        for (row, exposure) in toned
            .as_chunks_mut::<CHUNK_LANES>()
            .0
            .iter_mut()
            .zip(sums.iter())
        {
            row[lane] = shape.tone.get(tone_index(*exposure)).copied().unwrap_or(0);
        }
    }

    /// Write the pixel `columns` a chunk starting at sample column `first`
    /// owns, over pixel `rows`, from the chunk's toned samples: `samples` of
    /// them from sample row `top`, each row `used` lanes wide.
    fn write(
        &mut self,
        surface: &mut Surface,
        (first, columns): (u32, Range<u32>),
        (top, samples, rows): (u32, usize, Range<u32>),
        used: usize,
    ) {
        if columns.is_empty() || samples == 0 {
            return;
        }
        let sample_row = |row: usize| row * CHUNK_LANES..row * CHUNK_LANES + used;
        for row in rows {
            let upper = index_of(row / PIXELS_PER_SAMPLE - top).min(samples - 1);
            let above = self.toned.get(sample_row(upper)).unwrap_or_default();
            let lanes: &[u64] = if row % PIXELS_PER_SAMPLE == 0 {
                above
            } else {
                let below = self
                    .toned
                    .get(sample_row((upper + 1).min(samples - 1)))
                    .unwrap_or_default();
                let blended = self.between.get_mut(..used).unwrap_or_default();
                for ((blend, upper), lower) in blended.iter_mut().zip(above).zip(below) {
                    *blend = halve(*upper, *lower);
                }
                blended
            };
            let dither = &DITHER_LANES[index_of(row & 7)];
            let Some((start, span)) =
                surface.row_span_mut(row, columns.start, columns.end - columns.start)
            else {
                continue;
            };
            let lane = index_of(start / PIXELS_PER_SAMPLE - first);
            write_pairs(span, (start, lanes.get(lane..).unwrap_or_default()), dither);
        }
    }

    /// Lay out how the ribbon roams around the clock's text's clear space
    /// `exclusion`, already held to the screen: every path kept inside the
    /// screen, and beneath the text wherever its shadow falls the strands
    /// gathered and the course held low enough for their light to clear it.
    fn arrange(&mut self, exclusion: Rect) {
        self.exclusion = exclusion;
        let tall = f64::from(self.size.1);
        let clearance = self.clearance(exclusion);
        let (shadow, ceilings) = clearance.as_ref().map_or(
            ([0.0; CONTROLS], [0.0; CONTROLS]),
            |(clearance, ceilings)| (clearance.shadow, *ceilings),
        );
        let mut roam = Roam::default();
        let points = shadow.iter().zip(ceilings).enumerate();
        for (point, (shadow, ceiling)) in points {
            let gather = 1.0 - GATHER * shadow;
            let (above, beneath) = wander_extent(gather);
            let high = (EDGE_MARGIN + above) * tall;
            let held = high + shadow * mathf::fmax(ceiling + above * tall - high, 0.0);
            roam.high[point] = held;
            roam.low[point] = mathf::fmax((1.0 - EDGE_MARGIN - beneath) * tall, held);
            roam.gather[point] = gather;
        }
        roam.clearance = clearance.map(|(clearance, _)| clearance);
        self.roam = roam;
    }

    /// How the clock's text's clear space `exclusion` holds the ribbon clear,
    /// with each column beneath it answering a push in `yields`, and the
    /// highest a level path may run beneath the text at each control point,
    /// in pixels; `None` when there is no text.
    fn clearance(&mut self, exclusion: Rect) -> Option<(Clearance, [f64; CONTROLS])> {
        let (Ok(left), Ok(right), Ok(bottom)) = (
            u32::try_from(exclusion.left()),
            u32::try_from(exclusion.right()),
            u32::try_from(exclusion.bottom()),
        ) else {
            return None;
        };
        if exclusion.is_empty() {
            return None;
        }
        let (width, height) = self.size;
        let tall = f64::from(height);
        // A pixel between two samples blends both, so the text's last pixel
        // may reach the sample after it.
        let first = left / PIXELS_PER_SAMPLE;
        let last = ((right - 1) / PIXELS_PER_SAMPLE + 1).min(self.grid.0 - 1);
        let columns = index_of(first)..index_of(last) + 1;
        let wide = f64::from(width);
        let (from, to) = (f64::from(left) / wide, f64::from(right) / wide);
        let aspect = tall / wide;
        let (hold, half) = (SHADOW_HOLD * aspect, SHADOW_FADE * aspect * 0.5);
        let mut shadow = ABSCISSAE.map(|across| {
            let apart = mathf::fmax(mathf::fmax(from - across, across - to), 0.0);
            1.0 - f64::from(edge(narrow(apart - hold - half), narrow(half)))
        });
        self.answer(&shadow, columns.clone());
        // Where the shadow is too narrow for a push to lift a column clear of
        // the text as its paths steepen, it widens toward an even push, which
        // lifts every column alike and turns none.
        let widen = columns
            .clone()
            .map(|index| {
                let give = self.give(index, tall);
                if give < LEAST_GIVE {
                    (LEAST_GIVE - give) / (1.0 - give)
                } else {
                    0.0
                }
            })
            .fold(0.0, mathf::fmax);
        if widen > 0.0 {
            for weight in &mut shadow {
                *weight += widen * (1.0 - *weight);
            }
            self.answer(&shadow, columns.clone());
        }
        let dip = f64::from(edge_dip(narrow(tall * TOP_BLEND)));
        let below = f64::from(bottom) + f64::from(PIXELS_PER_SAMPLE) + CLEAR_SLACK;
        // Each control point is held clear of the light reaching up from the
        // columns about it; what the curve between them still needs, a push
        // gives.
        let near = 0.5 / f64::from(DEGREE);
        let mut ceilings = [0.0; CONTROLS];
        for (ceiling, across) in ceilings.iter_mut().zip(ABSCISSAE) {
            let about = columns.clone().filter(|index| {
                let at = sample_across(u32::try_from(*index).unwrap_or(u32::MAX), width);
                mathf::fabs(at - mathf::clamp(across, from, to)) <= near
            });
            let reach = about
                .map(|index| clear_reach(&self.columns[index], dip, tall))
                .fold(0.0, mathf::fmax);
            *ceiling = below + reach;
        }
        Some((
            Clearance {
                below,
                columns,
                shadow,
            },
            ceilings,
        ))
    }

    /// Lay out how each of sample `columns` answers a push down `shadow`.
    fn answer(&mut self, shadow: &[f64; CONTROLS], columns: Range<usize>) {
        let last = self.columns.len().saturating_sub(1);
        let lift = |index: usize| dot(&self.columns[index].basis, shadow);
        for index in columns {
            let (before, after, run) = neighbours(index, last);
            self.yields[index] = Yield {
                lift: lift(index),
                tilt: (lift(after) - lift(before)) / run,
            };
        }
    }

    /// The least sample column `index` moves clear of the text per pixel of
    /// push, whichever strand steepens, on a screen `tall` pixels high.
    fn give(&self, index: usize, tall: f64) -> f64 {
        let answer = self.yields[index];
        let widest = STRANDS
            .iter()
            .zip(self.columns[index].reach)
            .map(|(strand, reach)| rim(strand, reach, tall))
            .fold(0.0, mathf::fmax);
        answer.lift - widest * mathf::fabs(answer.tilt)
    }

    /// Place every sample column at `t` seconds: the paths traced and, where
    /// their light would reach the clock's text, pushed down its shadow; then
    /// the bright points, the glows and the upper edge, and the rows reached.
    fn place(&mut self, t: f64) {
        let tall = f64::from(self.size.1);
        self.paths = self.roam.paths(t, tall);
        self.trace();
        if let Some(clearance) = &self.roam.clearance {
            let pushes = needed_pushes(&self.columns, &self.placed, &self.yields, clearance, tall);
            if pushes.iter().any(|push| *push > 0.0) {
                for (path, push) in self.paths.iter_mut().zip(pushes) {
                    for (point, shadow) in path.iter_mut().zip(clearance.shadow) {
                        *point += push * shadow;
                    }
                }
                self.trace();
            }
        }
        self.shine(t);
        self.shape();
        let height = self.size.1;
        for (column, placed) in self.columns.iter().zip(&mut self.placed) {
            placed.lit = reached(column, placed, height);
        }
    }

    /// Run every strand's path through every sample column.
    fn trace(&mut self) {
        for (column, placed) in self.columns.iter().zip(&mut self.placed) {
            for (at, path) in placed.at.iter_mut().zip(&self.paths) {
                *at = narrow(dot(&column.basis, path));
            }
        }
    }

    /// Light every strand where its bright point's run carries it at `t`
    /// seconds.
    fn shine(&mut self, t: f64) {
        let mut sweep =
            STRANDS.map(|strand| Phasor::new(strand.sweep, t, (self.size.0, self.grid.0)));
        for (column, placed) in self.columns.iter().zip(&mut self.placed) {
            for (index, (strand, running)) in STRANDS.iter().zip(&mut sweep).enumerate() {
                let flare = strand.flare * point(running.value());
                placed.light[index] = narrow(f64::from(column.light[index]) * QUIET_LIGHT + flare);
                placed.curtain[index] = narrow(
                    f64::from(column.curtain[index]) * QUIET_LIGHT + flare * CURTAIN_FLARE_SHARE,
                );
                running.advance();
            }
        }
    }

    /// Widen each strand's glow by its slope, and run the ribbon's upper edge
    /// along whichever paths are highest.
    fn shape(&mut self) {
        let tall = f64::from(self.size.1);
        let blend = narrow(tall * TOP_BLEND);
        let last = self.placed.len().saturating_sub(1);
        for index in 0..self.placed.len() {
            let (before, after, run) = neighbours(index, last);
            let (was, next) = (self.placed[before].at, self.placed[after].at);
            let placed = &mut self.placed[index];
            let slopes = was
                .iter()
                .zip(next)
                .map(|(was, next)| f64::from(next - was) / run);
            for ((glow, strand), slope) in placed.glow.iter_mut().zip(&STRANDS).zip(slopes) {
                *glow = narrow(strand.glow * tall * widening(slope));
            }
            placed.top = placed
                .at
                .iter()
                .copied()
                .reduce(|top, at| smooth_min(top, at, blend))
                .unwrap_or_default();
        }
    }

    /// Record where the light falls now as where it fell.
    fn settle(&mut self) {
        for (drawn, placed) in self.drawn.iter_mut().zip(&self.placed) {
            *drawn = placed.lit;
        }
    }
}

impl Roam {
    /// Every strand's path at `t` seconds on a screen `tall` pixels high,
    /// before any push: the course the waves carry the ribbon along, and each
    /// strand in its lane, wandering on its own.
    fn paths(&self, t: f64, tall: f64) -> [[f64; CONTROLS]; STRAND_COUNT] {
        let mut course = [0.0; CONTROLS];
        for (point, (across, (high, low))) in course
            .iter_mut()
            .zip(ABSCISSAE.iter().zip(self.high.iter().zip(&self.low)))
        {
            let lift: f64 = COURSE.iter().map(|wave| wave.at(*across, t)).sum();
            *point = high + (low - high) * (1.0 + lift) * 0.5;
        }
        STRANDS.map(|strand| {
            let mut path = course;
            let points = path.iter_mut().zip(ABSCISSAE.iter().zip(&self.gather));
            for (point, (across, gather)) in points {
                let wander: f64 = strand.wander.iter().map(|wave| wave.at(*across, t)).sum();
                *point += (strand.lane + wander) * gather * tall;
            }
            path
        })
    }
}

/// How far each strand must be pushed down the text's shadow to hold its
/// light clear of the text, the paths as traced into `placed`: as far as the
/// column needing most needs, eased in as the need arises.
///
/// Per pixel of push a column moves by its lift and a path's slope there turns
/// by its tilt. A glow widens with its path's slope, but never by more than
/// the slope turns, so the push each column is owed is linear in its need and
/// is exact.
fn needed_pushes(
    columns: &[Column],
    placed: &[Placed],
    yields: &[Yield],
    clearance: &Clearance,
    tall: f64,
) -> [f64; STRAND_COUNT] {
    let last = placed.len().saturating_sub(1);
    let dip = f64::from(edge_dip(narrow(tall * TOP_BLEND)));
    let ease = PUSH_EASE * tall;
    let mut needs = [-ease; STRAND_COUNT];
    for index in clearance.columns.clone() {
        let (Some(column), Some(answer)) = (columns.get(index), yields.get(index)) else {
            continue;
        };
        let (before, after, run) = neighbours(index, last);
        let (was, here, next) = (placed[before].at, placed[index].at, placed[after].at);
        let flat = flat_rim(column, dip);
        for (strand_index, (strand, need)) in STRANDS.iter().zip(&mut needs).enumerate() {
            let at = f64::from(here[strand_index]);
            let slope = f64::from(next[strand_index] - was[strand_index]) / run;
            let rim = rim(strand, column.reach[strand_index], tall);
            let give = answer.lift - rim * mathf::fabs(answer.tilt);
            *need = mathf::fmax(*need, (clearance.below + flat - at) / answer.lift);
            *need = mathf::fmax(*need, (clearance.below + rim * widening(slope) - at) / give);
        }
    }
    needs.map(|need| f64::from(-smooth_min(narrow(-need), 0.0, narrow(ease))))
}

/// How far above the ribbon's highest level path the light of `column`
/// reaches, in pixels, on a screen `tall` pixels high, its upper edge dipping
/// up to `dip` pixels above that path.
fn clear_reach(column: &Column, dip: f64, tall: f64) -> f64 {
    STRANDS
        .iter()
        .zip(column.reach)
        .map(|(strand, reach)| rim(strand, reach, tall))
        .fold(flat_rim(column, dip), mathf::fmax)
}

/// How far above a level path its strand's light reaches, in pixels, on a
/// screen `tall` pixels high: its glow drawn `reach` radii out, or the curtain
/// easing in above it; a slope widens both alike.
fn rim(strand: &Strand, reach: f32, tall: f64) -> f64 {
    strand.glow * tall * mathf::fmax(f64::from(reach), 2.0 * CORE)
}

/// How far above the ribbon's highest path the light of `column` reaches
/// whatever the paths' slopes: its halo, drawn from an upper edge up to `dip`
/// pixels higher still, and the least ease of a curtain.
fn flat_rim(column: &Column, dip: f64) -> f64 {
    mathf::fmax(
        f64::from(column.halo_fall.0.reach) + dip,
        f64::from(LEAST_EASE),
    )
}

/// How far the ribbon's upper edge can stand above its highest path, in
/// pixels, for paths blended over `blend` pixels: each blend folding another
/// path in dips at most a quarter of `blend`.
fn edge_dip(blend: f32) -> f32 {
    let folds = u32::try_from(STRAND_COUNT.saturating_sub(1)).unwrap_or(u32::MAX);
    blend * 0.25 * narrow(f64::from(folds))
}

/// The most any strand's wandering carries it above the ribbon's course, and
/// the most beneath it, in screen heights, where the strands keep `gather` of
/// their lanes and their wandering.
fn wander_extent(gather: f64) -> (f64, f64) {
    STRANDS.iter().fold((0.0, 0.0), |(above, beneath), strand| {
        let wander: f64 = strand
            .wander
            .iter()
            .map(|wave| mathf::fabs(wave.amplitude))
            .sum();
        (
            mathf::fmax(above, (wander - strand.lane) * gather),
            mathf::fmax(beneath, (strand.lane + wander) * gather),
        )
    })
}

/// The sample columns either side of `index` a slope is taken across, the
/// last column being `last`, and how many pixels apart they stand.
fn neighbours(index: usize, last: usize) -> (usize, usize, f64) {
    let spacing = f64::from(PIXELS_PER_SAMPLE);
    let run = if index == 0 || index == last {
        spacing
    } else {
        2.0 * spacing
    };
    (index.saturating_sub(1), (index + 1).min(last), run)
}

/// How much a glow measured along the vertical widens on a path of `slope`.
fn widening(slope: f64) -> f64 {
    mathf::sqrt(1.0 + slope * slope)
}

/// Where along a path the screen stands `u` across its width.
fn along(u: f64) -> f64 {
    (u + OVERHANG) / (1.0 + 2.0 * OVERHANG)
}

/// The weight each control point of a Bézier curve takes at `t` along it.
fn bernstein(t: f64) -> [f64; CONTROLS] {
    let mut weights = [0.0; CONTROLS];
    weights[0] = 1.0;
    for degree in 1..CONTROLS {
        for point in (1..=degree).rev() {
            weights[point] = weights[point] * (1.0 - t) + weights[point - 1] * t;
        }
        weights[0] *= 1.0 - t;
    }
    weights
}

/// A path's height where a column's `basis` weighs its control `points`.
fn dot(basis: &[f32; CONTROLS], points: &[f64; CONTROLS]) -> f64 {
    basis
        .iter()
        .zip(points)
        .map(|(weight, point)| f64::from(*weight) * point)
        .sum()
}

/// A wave's value swept across the sample columns by turning its phase one
/// column at a time.
struct Phasor {
    amplitude: f64,
    sin: f64,
    cos: f64,
    step_sin: f64,
    step_cos: f64,
}

impl Phasor {
    /// `wave` at `t` seconds, at the first of `columns` sample columns across
    /// a `width`-pixel screen.
    fn new(wave: Wave, t: f64, (width, columns): (u32, u32)) -> Self {
        let wide = wave.wavelength * f64::from(width.max(1));
        let spacing = f64::from(PIXELS_PER_SAMPLE);
        let step = if columns > 1 {
            TAU * spacing / wide
        } else {
            0.0
        };
        let start = TAU * (0.5 / wide - t / wave.period) + wave.phase;
        Self {
            amplitude: wave.amplitude,
            sin: mathf::sin(start),
            cos: mathf::cos(start),
            step_sin: mathf::sin(step),
            step_cos: mathf::cos(step),
        }
    }

    fn value(&self) -> f64 {
        self.amplitude * self.sin
    }

    fn advance(&mut self) {
        let sin = self.sin * self.step_cos + self.cos * self.step_sin;
        self.cos = self.cos * self.step_cos - self.sin * self.step_sin;
        self.sin = sin;
    }
}

/// Sample `column`'s place across a `width`-pixel screen, in `0.0..1.0`: the
/// centre of the pixel it stands on.
fn sample_across(column: u32, width: u32) -> f64 {
    (f64::from(pixel_of(column)) + 0.5) / f64::from(width.max(1))
}

/// How much of its flare a strand shows where its bright point's run stands at
/// `sine`: all of it at the point, falling smoothly to none half a screen away.
fn point(sine: f64) -> f64 {
    let near = f64::midpoint(1.0, sine);
    near * near * near
}

/// What the sample column at `u` across the width holds still, on a screen
/// `tall` pixels high.
fn column_at(u: f64, tall: f64) -> Column {
    let halo = stations(HALO, u);
    let mut column = Column {
        basis: bernstein(along(u)).map(narrow),
        halo: narrow(halo),
        halo_fall: (
            Fall::new(stations(HALO_FALL, u) * tall, halo, OUTER_CUT),
            Fall::new(tall * INWARD_FALL, halo, TERM_CUT),
        ),
        ..Column::default()
    };
    for (index, strand) in STRANDS.iter().enumerate() {
        let light = stations(strand.light, u);
        column.light[index] = narrow(light);
        column.reach[index] = narrow(strand_reach(light * QUIET_LIGHT + strand.flare));
        let curtain = stations(strand.curtain, u);
        column.curtain[index] = narrow(curtain);
        let brightest = curtain * QUIET_LIGHT + strand.flare * CURTAIN_FLARE_SHARE;
        column.drape[index] = Fall::new(stations(strand.drape, u) * tall, brightest, TERM_CUT);
    }
    column
}

/// How high and how low a placed sample column's light reaches, in pixels.
fn light_bounds(column: &Column, placed: &Placed) -> (f32, f32) {
    let mut high = placed.top - column.halo_fall.0.reach;
    let mut low = placed.top + column.halo_fall.1.reach;
    for index in 0..STRAND_COUNT {
        let (at, glow) = (placed.at[index], placed.glow[index]);
        let spread = glow * column.reach[index];
        high = high.min(at - spread.max(curtain_ease(glow * narrow(CORE))));
        low = low.max(at + spread).max(at + column.drape[index].reach);
    }
    (high, low)
}

/// The pixel rows a placed sample column's light reaches, blending into its
/// neighbours included, on a screen `height` rows high.
fn reached(column: &Column, placed: &Placed, height: u32) -> (u32, u32) {
    let (high, low) = light_bounds(column, placed);
    let margin = narrow(f64::from(PIXELS_PER_SAMPLE));
    (
        row_at(high - margin, height),
        row_at(low + margin, height).saturating_add(1).min(height),
    )
}

/// Sum a placed column's light over sample `rows` into `sums`, one entry a
/// sample row.
fn gather(column: &Column, placed: &Placed, shape: &Shape, rows: Range<u32>, sums: &mut [f32]) {
    let rows = Rows {
        start: i64::from(rows.start),
        count: sums.len(),
    };
    for index in 0..STRAND_COUNT {
        curtain(
            (placed.curtain[index], column.drape[index]),
            (placed.at[index], placed.glow[index] * narrow(CORE)),
            rows,
            sums,
        );
    }
    glow_about((column.halo, placed.top), column.halo_fall, rows, sums);
    let Ok(profile) = <&[f32; PROFILE_LEN]>::try_from(shape.profile.as_slice()) else {
        return;
    };
    for index in 0..STRAND_COUNT {
        strand(
            (placed.light[index], column.reach[index]),
            (placed.at[index], placed.glow[index]),
            profile,
            rows,
            sums,
        );
    }
}

/// A run of consecutive sample rows, numbered down the screen from the first
/// sample row: the one it starts at, and how many it holds.
///
/// Sample row `r` is centred `PIXELS_PER_SAMPLE · r + 0.5` pixels down, on
/// the pixel it stands on.
#[derive(Copy, Clone, Debug)]
struct Rows {
    start: i64,
    count: usize,
}

impl Rows {
    fn end(self) -> i64 {
        self.start
            .saturating_add(i64::try_from(self.count).unwrap_or(i64::MAX))
    }

    /// The first sample row centred beyond `limit` pixels down.
    fn first_beyond(limit: f32) -> i64 {
        whole(mathf::floor(Self::samples_to(limit))) + 1
    }

    /// The first sample row centred at `limit` pixels down or beyond it.
    fn first_from(limit: f32) -> i64 {
        whole(mathf::ceil(Self::samples_to(limit)))
    }

    /// How many samples down from the first sample row's centre `limit` lies.
    fn samples_to(limit: f32) -> f64 {
        (f64::from(limit) - 0.5) / f64::from(PIXELS_PER_SAMPLE)
    }

    /// The centre of sample row `row`, in pixels.
    fn centre(row: i64) -> f32 {
        narrow(whole_f64(row) * f64::from(PIXELS_PER_SAMPLE) + 0.5)
    }

    /// The part of the sample rows `range` this run holds.
    fn held(self, range: Range<i64>) -> Range<i64> {
        let start = range.start.max(self.start);
        start..range.end.min(self.end()).max(start)
    }

    /// Where sample row `row`, which the run holds, sits among its sums.
    fn slot(self, row: i64) -> usize {
        usize::try_from(row - self.start).unwrap_or(0)
    }

    /// The sums of the sample rows `range`, which the run holds.
    fn sums<'a>(self, range: &Range<i64>, sums: &'a mut [f32]) -> &'a mut [f32] {
        let (from, to) = (self.slot(range.start), self.slot(range.end));
        sums.get_mut(from..to).unwrap_or_default()
    }
}

/// The centres of consecutive sample rows from `row` on.
fn centres(row: i64) -> impl Iterator<Item = f32> {
    let spacing = narrow(f64::from(PIXELS_PER_SAMPLE));
    successors(Some(Rows::centre(row)), move |centre| {
        Some(centre + spacing)
    })
}

/// A strand's curtain: its `light` hanging from its `path`, eased in about
/// the path over its `core` and falling off down its drape until it fades out.
///
/// The falloff always steps down from the first sample below the path, so a
/// sample's light does not depend on which rows were asked for.
fn curtain((light, drape): (f32, Fall), (path, core): (f32, f32), rows: Rows, sums: &mut [f32]) {
    if light <= 0.0 {
        return;
    }
    let ease = curtain_ease(core);
    let end = Rows::first_from(path + drape.reach);
    let below = Rows::first_beyond(path).min(end);
    let above = rows.held(Rows::first_beyond(path - ease)..below);
    for (sum, centre) in rows.sums(&above, sums).iter_mut().zip(centres(above.start)) {
        *sum += light * edge(centre - path, ease);
    }
    let hanging = rows.held(below..end);
    if hanging.is_empty() {
        return;
    }
    let mut fall = Falloff::from(Rows::centre(below) - path, drape);
    fall.skip_to(below, hanging.start);
    let eased = Rows::first_from(path + ease)
        .max(hanging.start)
        .min(hanging.end);
    let easing = hanging.start..eased;
    for (sum, centre) in rows
        .sums(&easing, sums)
        .iter_mut()
        .zip(centres(easing.start))
    {
        *sum += light * fall.value() * edge(centre - path, ease);
        fall.advance();
    }
    for sum in rows.sums(&(eased..hanging.end), sums) {
        *sum += light * fall.value();
        fall.advance();
    }
}

/// How far either side of its path a curtain eases in, for a strand whose
/// bright core is `core` pixels in radius.
fn curtain_ease(core: f32) -> f32 {
    (2.0 * core).max(LEAST_EASE)
}

/// A glow of `light` about the `line`, falling off upward by `above` and
/// downward by `below`: the rows centred above the line take the one, the
/// rest the other.
///
/// Each side steps out from the sample nearest the line, so a sample's light
/// does not depend on which rows were asked for.
fn glow_about(
    (light, line): (f32, f32),
    (above, below): (Fall, Fall),
    rows: Rows,
    sums: &mut [f32],
) {
    if light <= 0.0 {
        return;
    }
    let split = Rows::first_from(line);
    let upward = rows.held(Rows::first_beyond(line - above.reach)..split);
    if !upward.is_empty() {
        let nearest = split - 1;
        let mut fall = Falloff::from(line - Rows::centre(nearest), above);
        // Stepping upward, the rows beneath the run come first.
        fall.skip_to(upward.end - 1, nearest);
        for sum in rows.sums(&upward, sums).iter_mut().rev() {
            *sum += light * fall.value();
            fall.advance();
        }
    }
    let downward = rows.held(split..Rows::first_from(line + below.reach));
    if !downward.is_empty() {
        let mut fall = Falloff::from(Rows::centre(split) - line, below);
        fall.skip_to(split, downward.start);
        for sum in rows.sums(&downward, sums) {
            *sum += light * fall.value();
            fall.advance();
        }
    }
}

/// One strand's `light` about its `path`, drawn `reach` glow radii out, its
/// `glow` radius measured along the vertical.
fn strand(
    (light, reach): (f32, f32),
    (path, glow): (f32, f32),
    profile: &[f32; PROFILE_LEN],
    rows: Rows,
    sums: &mut [f32],
) {
    if light <= 0.0 || glow <= 0.0 || reach <= 0.0 {
        return;
    }
    let range =
        rows.held(Rows::first_beyond(path - glow * reach)..Rows::first_from(path + glow * reach));
    let steps = f32::from(PROFILE_STEPS) / glow;
    // Each row's distance from its own exact centre, so a row sums the same
    // light wherever the run asked for it begins.
    for (sum, centre) in rows.sums(&range, sums).iter_mut().zip(centres(range.start)) {
        let step = table_index((centre - path).abs() * steps).min(PROFILE_LEN - 1);
        *sum += light * profile[step];
    }
}

/// The ordered dither's biases spread across the lanes, for each of the eight
/// rows and eight columns the pattern repeats over.
const DITHER_LANES: [[u64; 8]; 8] = {
    let mut table = [[0; 8]; 8];
    let mut row = 0u32;
    while row < 8 {
        let dither = DitherRow::at(row);
        let mut column = 0u32;
        while column < 8 {
            table[row as usize][column as usize] = dither.bias(column) as u64 * LANE_ONES;
            column += 1;
        }
        row += 1;
    }
    table
};

/// Write `span`, whose first pixel is column `start`, from the toned samples
/// `lanes` its pixels stand on or between, the first the one at or before
/// `start`: a pixel on a sample takes it, one between two takes their mean.
fn write_pairs(span: &mut [Pixel], (start, lanes): (u32, &[u64]), dither: &[u64; 8]) {
    let bias = |x: u32| dither[index_of(x & 7)];
    let mut x = start;
    let mut pixels = span.iter_mut();
    let mut lanes = lanes.iter().copied();
    let Some(mut here) = lanes.next() else {
        return;
    };
    if x % PIXELS_PER_SAMPLE == 1 {
        let next = lanes.next().unwrap_or(here);
        if let Some(pixel) = pixels.next() {
            *pixel = shade(halve(here, next), bias(x));
        }
        here = next;
        x += 1;
    }
    let (pairs, rest) = pixels.into_slice().as_chunks_mut::<2>();
    for [on, between] in pairs {
        let next = lanes.next().unwrap_or(here);
        *on = shade(here, bias(x));
        *between = shade(halve(here, next), bias(x + 1));
        here = next;
        x += 2;
    }
    if let Some(pixel) = rest.first_mut() {
        *pixel = shade(here, bias(x));
    }
}

/// Two toned samples averaged lane by lane, rounding down.
fn halve(a: u64, b: u64) -> u64 {
    (a & b) + (((a ^ b) & !LANE_ONES) >> 1)
}

/// The pixel toned `lanes` show, rounded at the ordered dither's `bias`,
/// spread across the lanes.
fn shade(lanes: u64, bias: u64) -> Pixel {
    let bytes = (lanes + bias).to_le_bytes();
    Pixel {
        r: bytes[1],
        g: bytes[3],
        b: bytes[5],
        a: u8::MAX,
    }
}

/// The toning table entry `exposure` falls in.
fn tone_index(exposure: f32) -> usize {
    let knee = f32::from(KNEE);
    let fine = exposure.clamp(0.0, knee) * f32::from(FINE);
    let coarse = (exposure - knee).max(0.0) * f32::from(COARSE);
    table_index(fine + coarse).min(TONE_LEN - 1)
}

/// The toning table's entry `index`: the ember's colour at the exposure the
/// entry stands for, as 8.8 fixed-point channels in dithering lanes.
fn tone_entry(index: usize) -> u64 {
    if index == 0 {
        return 0;
    }
    let knee = usize::from(KNEE) * usize::from(FINE);
    let middle = |steps: usize| f64::from(u32::try_from(steps).unwrap_or(u32::MAX)) + 0.5;
    let exposure = if index < knee {
        middle(index) / f64::from(FINE)
    } else {
        f64::from(KNEE) + middle(index - knee) / f64::from(COARSE)
    };
    let luma = EMBER_TOP * (1.0 - mathf::exp(-exposure));
    let mut lanes = 0;
    for channel in 0..3 {
        let points = EMBER.map(|(at, colour)| (at, colour[channel]));
        let value = mathf::clamp(monotone(&points, luma), 0.0, 254.99);
        let fixed = u64::try_from(mathf::round_i32(value * 256.0)).unwrap_or(0);
        lanes |= fixed << (16 * channel);
    }
    lanes
}

/// The strand cross-section `step` profile steps from its path: a bright
/// core within a wider glow.
fn profile_at(step: usize) -> f32 {
    let distance = f64::from(u32::try_from(step).unwrap_or(u32::MAX)) / f64::from(PROFILE_STEPS);
    let core = distance / CORE;
    narrow(mathf::exp(-core * core) + GLOW_SHARE * mathf::exp(-distance * distance))
}

/// How far a strand as bright as `light` is drawn, in glow radii: where its
/// glow falls below one term's cut, never beyond the profile table.
fn strand_reach(light: f64) -> f64 {
    let peak = light * GLOW_SHARE;
    if peak <= TERM_CUT {
        return 0.0;
    }
    mathf::fmin(
        mathf::sqrt(mathf::ln(peak / TERM_CUT)),
        f64::from(STRAND_RADII),
    )
}

/// A smooth light falling off with distance, `(1 + x)·e^−x` for `x` the
/// distance over its length: flat where it starts, so a glow meets whatever
/// it grows from without a crease, and exponential beyond.
struct Falloff {
    x: f32,
    dx: f32,
    decay: f32,
    step: f32,
}

impl Falloff {
    /// The falloff `distance` pixels along `fall`, stepped a sample at a
    /// time.
    fn from(distance: f32, fall: Fall) -> Self {
        let length = f64::from(fall.length);
        let x = f64::from(distance) / length;
        Self {
            x: narrow(x),
            dx: narrow(f64::from(PIXELS_PER_SAMPLE) / length),
            decay: narrow(mathf::exp(-x)),
            step: fall.step,
        }
    }

    fn value(&self) -> f32 {
        (1.0 + self.x) * self.decay
    }

    fn advance(&mut self) {
        self.x += self.dx;
        self.decay *= self.step;
    }

    /// Step on from sample row `at` to sample row `to`, when `to` lies ahead.
    ///
    /// One sample at a time, exactly as a run that held those rows would have,
    /// so the light the falloff gives beyond them is the same either way.
    fn skip_to(&mut self, at: i64, to: i64) {
        for _ in at..to {
            self.advance();
        }
    }
}

/// How many lengths on a falloff of peak `light` stays below `cut`: the root
/// of `(1 + x)·e^−x = cut / light`, from above.
///
/// The root is where `x = b + ln(1 + x)` for `b = ln(light / cut)`. Since
/// `ln(1 + x) ≤ √x`, it lies below `b + ½ + √(b + ¼)`; stepping that bound
/// through the equation descends towards the root without passing it, so
/// every step is still a reach the light has fallen below `cut` by.
fn falloff_reach(light: f64, cut: f64) -> f64 {
    if light <= cut {
        return 0.0;
    }
    let base = mathf::ln(light / cut);
    let mut x = base + 0.5 + mathf::sqrt(base + 0.25);
    for _ in 0..4 {
        x = base + mathf::ln(1.0 + x);
    }
    x
}

/// A smooth step from nothing to all across `soft` either side of zero.
fn edge(offset: f32, soft: f32) -> f32 {
    let t = (offset / (2.0 * soft) + 0.5).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The lesser of `a` and `b`, blended over `blend` where they near each other.
fn smooth_min(a: f32, b: f32, blend: f32) -> f32 {
    if blend <= 0.0 {
        return a.min(b);
    }
    let h = (blend - (a - b).abs()).max(0.0) / blend;
    a.min(b) - h * h * blend * 0.25
}

/// The smooth curve through `points` at `u`, never going below nothing.
fn stations(points: Stations, u: f64) -> f64 {
    mathf::fmax(monotone(points, u), 0.0)
}

/// The monotone cubic (Fritsch–Carlson) through `points` at `x`: smooth, and
/// never overshooting a station, so a curve through stations of one sign
/// stays that sign. `x` beyond the stations holds the end values.
fn monotone(points: &[(f64, f64)], x: f64) -> f64 {
    let (Some(&(first, head)), Some(&(last, tail))) = (points.first(), points.last()) else {
        return 0.0;
    };
    if x <= first {
        return head;
    }
    if x >= last {
        return tail;
    }
    let segment = points
        .windows(2)
        .position(|pair| x < pair[1].0)
        .unwrap_or(points.len() - 2);
    let slope = |index: usize| {
        let (x0, y0) = points[index];
        let (x1, y1) = points[index + 1];
        (y1 - y0) / (x1 - x0)
    };
    let tangent = |index: usize| {
        if index == 0 {
            return slope(0);
        }
        if index == points.len() - 1 {
            return slope(index - 1);
        }
        let (before, after) = (slope(index - 1), slope(index));
        if before * after <= 0.0 {
            return 0.0;
        }
        let (h0, h1) = (
            points[index].0 - points[index - 1].0,
            points[index + 1].0 - points[index].0,
        );
        let (w0, w1) = (2.0 * h1 + h0, h1 + 2.0 * h0);
        (w0 + w1) / (w0 / before + w1 / after)
    };
    let (x0, y0) = points[segment];
    let (x1, y1) = points[segment + 1];
    let width = x1 - x0;
    let t = (x - x0) / width;
    let (t2, t3) = (t * t, t * t * t);
    (2.0 * t3 - 3.0 * t2 + 1.0) * y0
        + (t3 - 2.0 * t2 + t) * width * tangent(segment)
        + (-2.0 * t3 + 3.0 * t2) * y1
        + (t3 - t2) * width * tangent(segment + 1)
}

/// `area` held to a `size` screen.
fn on_screen(area: Rect, (width, height): (u32, u32)) -> Rect {
    area.intersection(&Rect::new(0, 0, width, height))
}

/// The rectangle of `columns` by `rows`.
fn rect(columns: Range<u32>, rows: Range<u32>) -> Rect {
    Rect::new(
        i32::try_from(columns.start).unwrap_or(i32::MAX),
        i32::try_from(rows.start).unwrap_or(i32::MAX),
        columns.end.saturating_sub(columns.start),
        rows.end.saturating_sub(rows.start),
    )
}

/// The indices `columns` names.
fn index_range(columns: Range<u32>) -> Range<usize> {
    index_of(columns.start)..index_of(columns.end)
}

/// The pixel sample `sample` stands on.
fn pixel_of(sample: u32) -> u32 {
    sample.saturating_mul(PIXELS_PER_SAMPLE)
}

/// A whole number held in a float, as an integer; beyond `i32` it saturates.
fn whole(value: f64) -> i64 {
    i64::from(mathf::round_i32(value))
}

/// A sample row as a float; beyond `i32` it saturates.
fn whole_f64(row: i64) -> f64 {
    f64::from(i32::try_from(row).unwrap_or(if row < 0 { i32::MIN } else { i32::MAX }))
}

/// A screen coordinate as an index; every one is far inside `usize`.
fn index_of(value: u32) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

/// The row a height of `at` pixels falls in, held to a `height`-row screen.
fn row_at(at: f32, height: u32) -> u32 {
    let row = mathf::clamp(mathf::floor(f64::from(at)), 0.0, f64::from(height));
    u32::try_from(mathf::round_i32(row)).unwrap_or(0)
}

/// A non-negative position in a table, rounded down.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "every caller passes a non-negative, finite position and clamps the \
              index into its table; the cast saturates rather than wrapping"
)]
fn table_index(position: f32) -> usize {
    position as usize
}

/// A length in pixels, or a light, at the precision a column keeps.
#[allow(
    clippy::cast_possible_truncation,
    reason = "screen lengths and exposures are small, and a pixel needs no \
              more than f32's precision"
)]
fn narrow(value: f64) -> f32 {
    value as f32
}

#[cfg(test)]
#[path = "light_tests.rs"]
mod tests;
