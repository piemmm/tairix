//! The ribbon of light behind the minimal clock.
//!
//! Five soft strands run from the left edge to the right: a crest, the band's
//! lower edge beneath it, and three between. The band pinches to nothing
//! where it twists and fans apart either side, and one strand rises across
//! the crest on the right. Each strand hangs a curtain of light beneath it
//! and the crest glows into the dark above. The terms add, so the light
//! builds where they overlap, and the sum is an exposure toned through an
//! ember's heat: deep red where it is faint, then orange, gold, and
//! near-white where it is hottest.
//!
//! The paths are Bézier curves across the width, every strand but the crest
//! placed at a fraction of the band beneath it, so the pinch and the crossing
//! hold however the ribbon moves. It moves as slow travelling waves: the
//! whole of it rising and falling, the band breathing, each inner strand
//! drifting within it. Every term falls off smoothly, so no edge is hard.
//!
//! The light is summed at every other pixel each way and blended back up to
//! every pixel as it is toned and dithered, and a frame repaints only the rows
//! it reaches in each column and those it reached the frame before.

use alloc::vec::Vec;
use core::f64::consts::TAU;
use core::iter::successors;
use core::ops::Range;

use tairix_raster::{DitherRow, Pixel, Surface};
use tairix_util::{fallible, mathf};
use tairix_wm::{Rect, Region};

/// The crest's height across the width, a quintic Bézier in screen heights.
const CREST: [f64; 6] = [0.833_66, 0.730_73, 0.781_54, 0.764_16, 0.749_69, 0.398_06];

/// Where across the width the band pinches to nothing as it twists.
const PINCH: f64 = 0.33;

/// The band's width beneath the crest: this quadratic Bézier times the square
/// of the distance from the pinch, in screen heights. Positive throughout, so
/// the band closes only at the pinch.
const SPREAD: [f64; 3] = [1.044_89, 1.728_77, 0.601_66];

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

/// How the whole ribbon rises and falls, in screen heights.
const SWELL: [Wave; 3] = [
    Wave {
        amplitude: 0.014,
        wavelength: 1.8,
        period: 26.0,
        phase: 0.4,
    },
    Wave {
        amplitude: 0.008,
        wavelength: 0.95,
        period: -17.0,
        phase: 1.9,
    },
    Wave {
        amplitude: 0.003,
        wavelength: 0.6,
        period: 11.0,
        phase: 4.1,
    },
];

/// How the band's width breathes, as a share of itself.
const BREATH: [Wave; 2] = [
    Wave {
        amplitude: 0.10,
        wavelength: 1.3,
        period: 21.0,
        phase: 0.7,
    },
    Wave {
        amplitude: 0.04,
        wavelength: 0.7,
        period: -13.0,
        phase: 0.0,
    },
];

/// Where a strand runs.
#[derive(Copy, Clone, Debug)]
enum Course {
    /// Along the crest.
    Crest,
    /// At this cubic Bézier's fraction of the band beneath the crest,
    /// drifting within it by the wave.
    Across([f64; 4], Wave),
    /// Along the band's lower edge.
    Edge,
}

/// Stations along the width, `(across, value)`, that a smooth curve passes
/// through: the first at the left edge, the last at the right.
type Stations = &'static [(f64, f64)];

/// One strand of the ribbon.
struct Strand {
    course: Course,
    /// The radius of its glow, in screen heights.
    glow: f64,
    /// How bright it is along the width, in exposure.
    light: Stations,
    /// The curtain it hangs beneath itself, in exposure; empty for none.
    curtain: Stations,
    /// How far its curtain falls, in screen heights.
    drape: Stations,
}

/// The ribbon's strands. The crest is first and the crossing strand second:
/// the band's top is whichever of the two is higher.
const STRANDS: [Strand; 5] = [
    Strand {
        course: Course::Crest,
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
        curtain: &[
            (0.0, 0.05),
            (0.15, 0.1),
            (0.28, 0.16),
            (0.33, 0.0),
            (0.42, 0.04),
            (0.6, 0.22),
            (0.75, 0.3),
            (0.85, 0.2),
            (0.95, 0.12),
            (1.0, 0.1),
        ],
        drape: &[
            (0.0, 0.072),
            (0.3, 0.048),
            (0.5, 0.021),
            (0.75, 0.024),
            (0.9, 0.042),
            (1.0, 0.054),
        ],
    },
    Strand {
        course: Course::Across(
            [0.239_09, 0.017_71, 0.559_98, -0.382_1],
            Wave {
                amplitude: 0.05,
                wavelength: 1.1,
                period: -19.0,
                phase: 2.0,
            },
        ),
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
        drape: &[
            (0.0, 0.018),
            (0.5, 0.024),
            (0.75, 0.027),
            (0.9, 0.06),
            (1.0, 0.072),
        ],
    },
    Strand {
        course: Course::Across(
            [0.483_34, 0.708_45, 0.057_23, 0.409_95],
            Wave {
                amplitude: 0.04,
                wavelength: 0.9,
                period: 23.0,
                phase: 0.3,
            },
        ),
        glow: 0.015,
        light: &[
            (0.0, 0.05),
            (0.2, 0.06),
            (0.33, 0.08),
            (0.5, 0.06),
            (0.6, 0.1),
            (0.7, 0.125),
            (0.75, 0.14),
            (0.85, 0.125),
            (0.95, 0.1),
            (1.0, 0.085),
        ],
        curtain: &[
            (0.0, 0.03),
            (0.3, 0.03),
            (0.5, 0.04),
            (0.65, 0.09),
            (0.75, 0.12),
            (0.9, 0.1),
            (1.0, 0.08),
        ],
        drape: &[
            (0.0, 0.020),
            (0.4, 0.024),
            (0.75, 0.027),
            (0.95, 0.036),
            (1.0, 0.042),
        ],
    },
    Strand {
        course: Course::Across(
            [0.756_78, 0.712_38, 1.174_02, 0.589_09],
            Wave {
                amplitude: 0.04,
                wavelength: 1.4,
                period: -29.0,
                phase: 5.0,
            },
        ),
        glow: 0.014,
        light: &[
            (0.0, 0.035),
            (0.2, 0.04),
            (0.33, 0.05),
            (0.5, 0.03),
            (0.66, 0.03),
            (0.8, 0.035),
            (0.9, 0.03),
            (1.0, 0.025),
        ],
        curtain: &[
            (0.0, 0.015),
            (0.5, 0.015),
            (0.75, 0.03),
            (0.9, 0.03),
            (1.0, 0.02),
        ],
        drape: &[(0.0, 0.015), (0.6, 0.024), (1.0, 0.03)],
    },
    Strand {
        course: Course::Edge,
        glow: 0.015,
        light: &[
            (0.0, 0.013),
            (0.1, 0.042),
            (0.2, 0.14),
            (0.3, 0.2),
            (0.33, 0.16),
            (0.4, 0.04),
            (0.6, 0.04),
            (0.8, 0.036),
            (1.0, 0.018),
        ],
        curtain: &[],
        drape: &[],
    },
];

/// Strands in the ribbon.
const STRAND_COUNT: usize = STRANDS.len();

/// The glow into the dark above the band, in exposure.
const HALO: Stations = &[
    (0.0, 0.0),
    (0.15, 0.03),
    (0.3, 0.09),
    (0.45, 0.2),
    (0.6, 0.3),
    (0.75, 0.3),
    (0.85, 0.28),
    (0.95, 0.18),
    (1.0, 0.15),
];

/// How far the glow above falls off, in screen heights.
const HALO_FALL: Stations = &[
    (0.0, 0.012),
    (0.3, 0.019),
    (0.6, 0.027),
    (0.75, 0.03),
    (0.9, 0.031),
    (1.0, 0.031),
];

/// The faint light that fills the band from its top to its lower edge, in
/// exposure.
const FLOOR: Stations = &[
    (0.0, 0.05),
    (0.15, 0.06),
    (0.25, 0.08),
    (0.31, 0.04),
    (0.4, 0.015),
    (0.6, 0.03),
    (0.75, 0.035),
    (0.9, 0.035),
    (1.0, 0.03),
];

/// The glow beneath the band where it narrows to the pinch, in exposure.
const UNDERGLOW: Stations = &[
    (0.0, 0.0),
    (0.15, 0.05),
    (0.3, 0.12),
    (0.4, 0.05),
    (0.55, 0.0),
    (1.0, 0.0),
];

/// How far the glow beneath falls off, in screen heights.
const UNDERGLOW_FALL: f64 = 0.012;

/// How far either glow falls off into the band itself, in screen heights.
const INWARD_FALL: f64 = 0.006;

/// How wide the band's softened top and lower edge are, in screen heights.
const SOFT_EDGE: f64 = 0.022;

/// How far apart the crest and the crossing strand blend into one top edge,
/// in screen heights, so the band's top turns smoothly where they cross.
const TOP_BLEND: f64 = 0.01;

/// A strand's bright core, as a share of its glow's radius, and the share of
/// its light the glow carries.
const CORE: f64 = 0.4;
const GLOW_SHARE: f64 = 0.48;

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

/// The faintest a glow is drawn to outside the band, where nothing else's
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

/// The luma an unbounded exposure approaches.
const EMBER_TOP: f64 = 244.0;

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

/// Sample columns whose pixels are repainted as one strip.
const STRIP: u32 = 32;

/// What a sample column holds still: its place on the ribbon at rest, its
/// light, and how far each term of it reaches.
#[derive(Copy, Clone, Debug, Default)]
struct Column {
    /// The crest's height at rest, in pixels.
    crest: f32,
    /// The band's width at rest, in pixels.
    spread: f32,
    /// Each strand's fraction of the band beneath the crest.
    across: [f32; STRAND_COUNT],
    light: [f32; STRAND_COUNT],
    /// How far each strand is drawn from its path, in glow radii.
    reach: [f32; STRAND_COUNT],
    curtain: [f32; STRAND_COUNT],
    drape: [Fall; STRAND_COUNT],
    halo: f32,
    /// The glow above falling off upward, and into the band.
    halo_fall: (Fall, Fall),
    floor: f32,
    underglow: f32,
    /// The glow beneath falling off into the band, and downward.
    underglow_fall: (Fall, Fall),
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
    /// Each strand's glow radius along the vertical, widened by its slope.
    glow: [f32; STRAND_COUNT],
    top: f32,
    bottom: f32,
    /// The pixel rows the light reaches, blending included.
    lit: (u32, u32),
}

/// What every column shares: the tables, and the band's softened edge in
/// pixels.
struct Shape {
    /// The strand cross-section by distance from the path, in glow radii.
    profile: Vec<f32>,
    /// Exposure to dithering lanes of 8.8 fixed-point channels.
    tone: Vec<u64>,
    soft: f32,
}

/// The ribbon: its sample columns at rest, where their light falls this frame
/// and fell the last, the strips a frame repaints, and the scratch light is
/// summed and toned in.
pub(super) struct Light {
    /// The screen, in pixels.
    size: (u32, u32),
    /// The samples, one for every two pixels each way.
    grid: (u32, u32),
    columns: Vec<Column>,
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
    /// The ribbon for a `size` screen, placed as it stands at `t` seconds, or
    /// `None` when the screen is empty or the heap will not give it.
    pub(super) fn new(size: (u32, u32), t: f64) -> Option<Self> {
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
        let wide = index_of(CHUNK + 1);
        let mut columns = Vec::new();
        let mut placed = Vec::new();
        let mut drawn = Vec::new();
        let mut strips = Vec::new();
        let mut sums = Vec::new();
        let mut toned = Vec::new();
        let mut between = Vec::new();
        let mut tone = Vec::new();
        let mut profile = Vec::new();
        if !(fallible::reserve(&mut columns, count)
            && fallible::grow_to(&mut placed, count, Placed::default())
            && fallible::grow_to(&mut drawn, count, (0, 0))
            && fallible::reserve(&mut strips, index_of(grid.0.div_ceil(STRIP)))
            && fallible::grow_to(&mut sums, depth, 0.0)
            && fallible::grow_to(&mut toned, wide.checked_mul(depth)?, 0)
            && fallible::grow_to(&mut between, wide, 0)
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
            grid,
            columns,
            placed,
            drawn,
            strips,
            shape: Shape {
                profile,
                tone,
                soft: narrow(tall * SOFT_EDGE),
            },
            sums,
            toned,
            between,
        };
        light.place(t);
        light.settle();
        Some(light)
    }

    /// Move the ribbon to `t` seconds, adding to `damage` every pixel whose
    /// light may have changed — those it reaches now and those it reached —
    /// and keeping them as the strips [`paint_moved`](Self::paint_moved)
    /// repaints.
    pub(super) fn step(&mut self, t: f64, damage: &mut Region) {
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
                // A sample colours the pixels either side of its own, which
                // the next strip's samples also blend into.
                let left = pixel_of(start).saturating_sub(1);
                let right = pixel_of(end).saturating_add(1).min(width);
                let strip = rect(left..right, rows.0..rows.1.min(height));
                damage.add(strip);
                self.strips.push(strip);
            }
            start = end;
        }
        self.settle();
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
        let wide = index_of(CHUNK + 1);
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
            for row in self.toned.chunks_exact_mut(wide).take(depth) {
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
        let wide = index_of(CHUNK + 1);
        for (row, exposure) in toned.chunks_exact_mut(wide).zip(sums.iter()) {
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
        let wide = index_of(CHUNK + 1);
        let sample_row = |row: usize| row * wide..row * wide + used;
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
            let dither = dither_lanes(row);
            let Some((start, span)) =
                surface.row_span_mut(row, columns.start, columns.end - columns.start)
            else {
                continue;
            };
            let lane = index_of(start / PIXELS_PER_SAMPLE - first);
            write_pairs(
                span,
                (start, lanes.get(lane..).unwrap_or_default()),
                &dither,
            );
        }
    }

    /// Place every sample column at `t` seconds: the waves first, then each
    /// strand's glow widened by its slope, the band's edges, and the rows
    /// reached.
    fn place(&mut self, t: f64) {
        let (width, height) = self.size;
        let tall = f64::from(height);
        let columns = self.grid.0;
        let mut swell = SWELL.map(|wave| Phasor::new(wave, t, (width, columns)));
        let mut breath = BREATH.map(|wave| Phasor::new(wave, t, (width, columns)));
        let mut drift = STRANDS.map(|strand| match strand.course {
            Course::Across(_, wave) => Some(Phasor::new(wave, t, (width, columns))),
            Course::Crest | Course::Edge => None,
        });
        for (column, placed) in self.columns.iter().zip(self.placed.iter_mut()) {
            let rise = swell.iter().map(Phasor::value).sum::<f64>() * tall;
            let breathe = 1.0 + breath.iter().map(Phasor::value).sum::<f64>();
            let crest = f64::from(column.crest) + rise;
            let spread = f64::from(column.spread) * breathe;
            for ((at, share), drifting) in placed.at.iter_mut().zip(column.across).zip(&drift) {
                let offset = drifting.as_ref().map_or(0.0, Phasor::value);
                *at = narrow(crest + (f64::from(share) + offset) * spread);
            }
            for phasor in swell.iter_mut().chain(breath.iter_mut()) {
                phasor.advance();
            }
            for phasor in drift.iter_mut().flatten() {
                phasor.advance();
            }
        }
        let blend = narrow(tall * TOP_BLEND);
        let spacing = f64::from(PIXELS_PER_SAMPLE);
        let last = self.placed.len().saturating_sub(1);
        for index in 0..self.placed.len() {
            let before = self.placed[index.saturating_sub(1)].at;
            let after = self.placed[(index + 1).min(last)].at;
            let run = if index == 0 || index == last {
                spacing
            } else {
                2.0 * spacing
            };
            let placed = &mut self.placed[index];
            for (strand_index, strand) in STRANDS.iter().enumerate() {
                let slope = f64::from(after[strand_index] - before[strand_index]) / run;
                placed.glow[strand_index] =
                    narrow(strand.glow * tall * mathf::sqrt(1.0 + slope * slope));
            }
            placed.top = smooth_min(placed.at[0], placed.at[1], blend);
            placed.bottom = placed.at[STRAND_COUNT - 1].max(placed.top);
            placed.lit = reached(&self.columns[index], placed, self.shape.soft, height);
        }
    }

    /// Record where the light falls now as where it fell.
    fn settle(&mut self) {
        for (drawn, placed) in self.drawn.iter_mut().zip(&self.placed) {
            *drawn = placed.lit;
        }
    }
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

/// What the sample column at `u` across the width holds still, on a screen
/// `tall` pixels high.
fn column_at(u: f64, tall: f64) -> Column {
    let halo = stations(HALO, u);
    let underglow = stations(UNDERGLOW, u);
    let inward = tall * INWARD_FALL;
    let mut column = Column {
        crest: narrow(bezier(&CREST, u) * tall),
        spread: narrow((u - PINCH) * (u - PINCH) * bezier(&SPREAD, u) * tall),
        halo: narrow(halo),
        halo_fall: (
            Fall::new(stations(HALO_FALL, u) * tall, halo, OUTER_CUT),
            Fall::new(inward, halo, TERM_CUT),
        ),
        floor: narrow(stations(FLOOR, u)),
        underglow: narrow(underglow),
        underglow_fall: (
            Fall::new(inward, underglow, TERM_CUT),
            Fall::new(tall * UNDERGLOW_FALL, underglow, OUTER_CUT),
        ),
        ..Column::default()
    };
    for (index, strand) in STRANDS.iter().enumerate() {
        column.across[index] = match strand.course {
            Course::Crest => 0.0,
            Course::Across(fraction, _) => narrow(bezier(&fraction, u)),
            Course::Edge => 1.0,
        };
        let light = stations(strand.light, u);
        column.light[index] = narrow(light);
        column.reach[index] = narrow(strand_reach(light));
        if !strand.curtain.is_empty() {
            let curtain = stations(strand.curtain, u);
            column.curtain[index] = narrow(curtain);
            column.drape[index] = Fall::new(stations(strand.drape, u) * tall, curtain, TERM_CUT);
        }
    }
    column
}

/// The pixel rows a placed sample column's light reaches, blending into its
/// neighbours included, `soft` the band's softened edge, on a screen `height`
/// rows high.
fn reached(column: &Column, placed: &Placed, soft: f32, height: u32) -> (u32, u32) {
    let mut high = placed.top - soft.max(column.halo_fall.0.reach);
    let mut low = placed.bottom + soft.max(column.underglow_fall.1.reach);
    low = low.max(placed.top + column.halo_fall.1.reach);
    high = high.min(placed.bottom - column.underglow_fall.0.reach);
    for ((at, glow), reach) in placed.at.iter().zip(placed.glow).zip(column.reach) {
        high = high.min(at - glow * reach);
        low = low.max(at + glow * reach);
    }
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
    let band = Band {
        top: placed.top,
        bottom: placed.bottom,
        soft: shape.soft,
    };
    band.floor(column.floor, rows, sums);
    for index in 0..STRAND_COUNT {
        band.curtain(
            (column.curtain[index], column.drape[index]),
            (placed.at[index], placed.glow[index] * narrow(CORE)),
            rows,
            sums,
        );
    }
    band.fade_bottom(rows, sums);
    glow_about((column.halo, placed.top), column.halo_fall, rows, sums);
    glow_about(
        (column.underglow, placed.bottom),
        column.underglow_fall,
        rows,
        sums,
    );
    let Ok(profile) = <&[f32; PROFILE_LEN]>::try_from(shape.profile.as_slice()) else {
        return;
    };
    for index in 0..STRAND_COUNT {
        strand(
            (column.light[index], column.reach[index]),
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

/// The band between the top edge and the lower one, softened at both.
struct Band {
    top: f32,
    bottom: f32,
    soft: f32,
}

impl Band {
    /// The faint light filling the band, eased in across its top.
    fn floor(&self, light: f32, rows: Rows, sums: &mut [f32]) {
        if light <= 0.0 {
            return;
        }
        let eased = Rows::first_from(self.top + self.soft);
        let easing = rows.held(Rows::first_beyond(self.top - self.soft)..eased);
        for (sum, centre) in rows
            .sums(&easing, sums)
            .iter_mut()
            .zip(centres(easing.start))
        {
            *sum += light * edge(centre - self.top, self.soft);
        }
        let full = rows.held(eased..Rows::first_from(self.bottom + self.soft));
        for sum in rows.sums(&full, sums) {
            *sum += light;
        }
    }

    /// A strand's curtain: its `light` hanging from its `path`, eased in over
    /// twice its `core` and falling off down its drape until it fades out or
    /// the band ends.
    ///
    /// The falloff always steps down from the first sample below the path,
    /// so a sample's light does not depend on which rows were asked for.
    fn curtain(
        &self,
        (light, drape): (f32, Fall),
        (path, core): (f32, f32),
        rows: Rows,
        sums: &mut [f32],
    ) {
        if light <= 0.0 {
            return;
        }
        let ease = (2.0 * core).max(0.5);
        let end = Rows::first_from((self.bottom + self.soft).min(path + drape.reach));
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

    /// Fade the band's light out across its lower edge.
    fn fade_bottom(&self, rows: Rows, sums: &mut [f32]) {
        let range = rows.held(
            Rows::first_beyond(self.bottom - self.soft)..Rows::first_from(self.bottom + self.soft),
        );
        for (sum, centre) in rows.sums(&range, sums).iter_mut().zip(centres(range.start)) {
            *sum *= edge(self.bottom - centre, self.soft);
        }
    }
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

/// Row `row`'s ordered-dither biases, spread across the lanes, for each of
/// the eight columns the pattern repeats over.
fn dither_lanes(row: u32) -> [u64; 8] {
    let dither = DitherRow::at(row);
    core::array::from_fn(|column| {
        u64::from(dither.bias(u32::try_from(column).unwrap_or(0))) * LANE_ONES
    })
}

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

/// A Bézier curve of `points` control values at `t`, by de Casteljau.
fn bezier<const N: usize>(points: &[f64; N], t: f64) -> f64 {
    let mut values = *points;
    for level in (1..N).rev() {
        for index in 0..level {
            values[index] += (values[index + 1] - values[index]) * t;
        }
    }
    values[0]
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
