//! A window's effect pipeline: the passes' stages, kept between frames and
//! brought up to date over what changed.
//!
//! The passes fall into three stages by what an output pixel depends on.
//! Wobble, phosphor and scan lines read one screen pixel and the row it is on
//! (`pre`); glow reads the neighbourhood of `pre` its reach spans (`lit`);
//! fuzz reads one pixel of `lit` and the step (`out`). A stage exists only
//! while one of its passes is on, and each render redraws a stage only where
//! its input changed, so a keystroke costs its cells plus the glow's reach
//! around them and an unchanging screen costs nothing.
//!
//! The phosphor trail decays once per animation step, never per present, so
//! it fades in time as a tube does. It settles once every pixel's trail is the
//! level that pixel's own light holds it at; from then on nothing animates
//! until the screen changes again.

use alloc::vec::Vec;
use core::mem;

use tairix_geometry::{to_i32, Rect, Region};
use tairix_mix::{mix64, GOLDEN_GAMMA};
use tairix_parallel::{for_each_drawn, JobRunner};
use tairix_raster::{box_blur, Pixel, Surface};

use crate::effects::{Effects, Pass, Phase, FULL};

/// Pixels below which a stage is not worth splitting across cores.
const MIN_PARALLEL_PX: usize = 64 * 1024;

/// How much of a remembered trail is added back to each channel: a third, so
/// it reads as a dimming ghost rather than a second copy of the text.
const TRAIL_SHARE: u32 = 3;

/// How hard a pixel must be driven before it glows at all, on the 8-bit
/// channel scale.
///
/// Half scale: text of any colour clears it, an unlit or mid-grey background
/// does not, so the halo comes off the text rather than washing the field.
const GLOW_KNEE: u8 = 128;

const _: () = assert!(GLOW_KNEE < u8::MAX);

/// The rows one hull covers.
///
/// A trail step's report and a change the wobble displaces are each one
/// rectangle per this many rows, which keeps the regions they feed small —
/// a strip per row would be blurred from its own neighbourhood each — without
/// merging distant lines of text into one box.
const HULL_ROWS: u32 = 8;

/// The passes in force, resolved to the stage each belongs to.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
struct Params {
    /// The wobble's peak displacement, in physical pixels.
    wobble: Option<u32>,
    /// How much of a lit pixel survives one step, in permille.
    phosphor: Option<u32>,
    /// What a scan line keeps of the row it dims, out of 255.
    scan: Option<u8>,
    glow: Option<Glow>,
    /// The fuzz's peak jitter, as an 8-bit amplitude.
    fuzz: Option<u32>,
}

/// The glow's resolved reach and gain.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Glow {
    reach: u32,
    intensity: u32,
}

impl Params {
    fn of(effects: Effects, scale_percent: u32) -> Self {
        let (passes, len) = effects.passes(scale_percent);
        let mut params = Self::default();
        for pass in passes.iter().take(len) {
            match *pass {
                Pass::Wobble { amplitude_px } => params.wobble = Some(amplitude_px),
                Pass::Phosphor { persistence } => params.phosphor = Some(persistence),
                Pass::ScanLines { depth } => params.scan = Some(scan_keep(depth)),
                Pass::Glow {
                    reach_px,
                    intensity,
                } => {
                    params.glow = Some(Glow {
                        reach: reach_px,
                        intensity,
                    });
                }
                Pass::Fuzz { amplitude } => params.fuzz = Some(amplitude),
            }
        }
        params
    }

    const fn is_empty(&self) -> bool {
        !self.shades() && self.glow.is_none() && self.fuzz.is_none()
    }

    /// Whether the `pre` stage exists.
    const fn shades(&self) -> bool {
        self.wobble.is_some() || self.phosphor.is_some() || self.scan.is_some()
    }

    /// Whether some pass draws differently at every step whatever the screen
    /// shows.
    const fn moves(&self) -> bool {
        self.wobble.is_some() || self.fuzz.is_some()
    }
}

/// What a scan line of `depth` permille keeps of the row it dims, out of 255.
fn scan_keep(depth: u32) -> u8 {
    let keep = u32::from(FULL).saturating_sub(depth.min(u32::from(FULL)));
    u8::try_from(keep * 255 / u32::from(FULL)).unwrap_or(u8::MAX)
}

/// The screen and the passes the stages were last brought up to date for.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Drawn {
    width: u32,
    height: u32,
    params: Params,
}

/// What a render produced.
#[derive(Clone, Copy, Debug)]
pub struct Frame<'a> {
    /// The effected picture, the window's whole extent.
    pub picture: &'a Surface,
    /// The part of `picture` that differs from what the previous render
    /// produced; empty when nothing does.
    pub changed: &'a Region,
}

/// One render's inputs, as every stage reads them.
#[derive(Copy, Clone)]
struct Step<'a> {
    screen: &'a Surface,
    damage: Rect,
    bounds: Rect,
    params: Params,
    phase: Phase,
    /// The whole picture is redrawn: a new screen, new strengths, or damage
    /// covering all of it.
    whole: bool,
    /// The clock moved since the last render.
    stepped: bool,
    runner: &'a dyn JobRunner,
}

/// One window's effect pipeline.
#[derive(Debug, Default)]
pub struct Pipeline {
    drawn: Option<Drawn>,
    /// The step the stages show.
    phase: Phase,
    pre: Option<Surface>,
    lit: Option<Surface>,
    out: Option<Surface>,
    trail: Trail,
    /// The wobble's displacement of each row as `pre` shows it.
    offsets: Vec<i32>,
    halo: Halo,
    /// Where `pre` was redrawn by the last render.
    shaded: Region,
    /// Where `lit` was redrawn by the last render.
    glowed: Region,
    /// Where the picture changed in the last render.
    changed: Region,
}

impl Pipeline {
    /// A pipeline that has drawn nothing.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget everything drawn and remembered, and give the buffers back.
    ///
    /// For a screen that has stopped existing — a resize, new colours or a
    /// new face, memory pressure — where a trail of the old picture would be
    /// a ghost of the wrong thing.
    pub fn clear(&mut self) {
        *self = Self::new();
    }

    /// Forget everything as [`clear`](Self::clear) does, answering whether an
    /// effected picture was drawn: the window shows it until the plain screen
    /// replaces it whole.
    pub fn retire(&mut self) -> bool {
        let drawn = self.drawn.is_some();
        self.clear();
        drawn
    }

    /// Whether this window needs a timed step to be seen moving: a wobble or
    /// a fuzz is on, or a phosphor trail has not yet settled.
    #[must_use]
    pub fn animated(&self, effects: Effects, scale_percent: u32) -> bool {
        let params = Params::of(effects, scale_percent);
        params.moves() || (params.phosphor.is_some() && self.trail.unsettled())
    }

    /// Bring the effected picture up to date with `screen`, of which only
    /// `damage` changed since the previous render, at step `phase`.
    ///
    /// `None` when no pass is on, or when the process cannot afford the
    /// stages — the caller then presents the plain screen, so a refused
    /// buffer costs the effect, never the terminal.
    pub fn render(
        &mut self,
        screen: &Surface,
        damage: Rect,
        effects: Effects,
        scale_percent: u32,
        phase: Phase,
        runner: &dyn JobRunner,
    ) -> Option<Frame<'_>> {
        let params = Params::of(effects, scale_percent);
        if params.is_empty() {
            self.clear();
            return None;
        }
        let (width, height) = (screen.width(), screen.height());
        let bounds = Rect::new(0, 0, width, height);
        let previous = self.drawn;
        let reshaped = previous.is_none_or(|drawn| (drawn.width, drawn.height) != (width, height));
        if reshaped {
            self.trail = Trail::default();
            self.offsets = Vec::new();
        }
        let whole = reshaped
            || previous.is_none_or(|drawn| drawn.params != params)
            || damage.intersection(&bounds) == bounds;
        let stepped = previous.is_some() && phase != self.phase;
        if !self.fit(params, width, height) {
            self.clear();
            return None;
        }
        let step = Step {
            screen,
            damage,
            bounds,
            params,
            phase,
            whole,
            stepped,
            runner,
        };
        self.shade_stage(&step);
        self.glow_stage(&step);
        self.fuzz_stage(&step);
        self.drawn = Some(Drawn {
            width,
            height,
            params,
        });
        self.phase = phase;
        let picture = self
            .out
            .as_ref()
            .or(self.lit.as_ref())
            .or(self.pre.as_ref())?;
        Some(Frame {
            picture,
            changed: &self.changed,
        })
    }

    /// Redraw `pre` where the picture moved — the damage, displaced where the
    /// rows wobble, every row a wobble step moved, and where the phosphor
    /// trail stepped — recording that in `shaded`.
    fn shade_stage(&mut self, step: &Step<'_>) {
        let Step {
            screen,
            damage,
            bounds,
            params,
            phase,
            whole,
            stepped,
            runner,
        } = *step;
        let mut shaded = mem::take(&mut self.shaded);
        shaded.clear();
        if whole {
            shaded.add(bounds);
        }
        if let Some(amplitude) = params.wobble {
            if whole {
                for (y, offset) in (0u32..).zip(self.offsets.iter_mut()) {
                    *offset = wobble_offset(y, amplitude, phase);
                }
            } else if stepped {
                shift_rows(
                    &mut self.offsets,
                    amplitude,
                    phase,
                    screen.width(),
                    &mut shaded,
                );
            }
        }
        if !whole {
            displace_damage(
                &mut shaded,
                damage.intersection(&bounds),
                &self.offsets,
                bounds,
            );
        }
        if let Some(persistence) = params.phosphor {
            for rect in shaded.rects() {
                self.trail.pending.add(*rect);
            }
            if stepped {
                self.trail
                    .step(screen, &self.offsets, persistence, &mut shaded);
            }
        }
        if let Some(pre) = self.pre.as_mut() {
            shade(
                pre,
                screen,
                &shaded,
                &Shading {
                    offsets: &self.offsets,
                    trail: params.phosphor.map(|_| self.trail.levels.as_slice()),
                    scan: params.scan,
                },
                runner,
            );
        }
        self.shaded = shaded;
    }

    /// Redraw `lit` over what `pre` changed widened by the glow's reach,
    /// recording that in `glowed`.
    fn glow_stage(&mut self, step: &Step<'_>) {
        let mut glowed = mem::take(&mut self.glowed);
        glowed.clear();
        if let (Some(glow), Some(lit)) = (step.params.glow, self.lit.as_mut()) {
            for rect in self.shaded.rects() {
                glowed.add(rect.outset(glow.reach).intersection(&step.bounds));
            }
            let source = self.pre.as_ref().unwrap_or(step.screen);
            self.halo.glow(lit, source, &glowed, glow);
        } else {
            glowed.clone_from(&self.shaded);
        }
        self.glowed = glowed;
    }

    /// Redraw `out` over what the glow changed, or the whole picture on a
    /// step the fuzz moves, recording that in `changed`.
    fn fuzz_stage(&mut self, step: &Step<'_>) {
        let mut changed = mem::take(&mut self.changed);
        changed.clear();
        if let Some(amplitude) = step.params.fuzz {
            if step.whole || step.stepped {
                changed.add(step.bounds);
            } else {
                changed.clone_from(&self.glowed);
            }
            if let Some(out) = self.out.as_mut() {
                let source = self
                    .lit
                    .as_ref()
                    .or(self.pre.as_ref())
                    .unwrap_or(step.screen);
                fuzz(out, source, &changed, amplitude, step.phase, step.runner);
            }
        } else {
            changed.clone_from(&self.glowed);
        }
        self.changed = changed;
    }

    /// Hold exactly the stages `params` needs at `width` × `height`, or
    /// report that the process cannot afford them.
    fn fit(&mut self, params: Params, width: u32, height: u32) -> bool {
        let rows = usize::try_from(height).unwrap_or(usize::MAX);
        let offsets = if params.wobble.is_some() {
            refit(&mut self.offsets, rows, 0)
        } else {
            self.offsets = Vec::new();
            true
        };
        offsets
            && fit_stage(&mut self.pre, params.shades(), width, height)
            && fit_stage(&mut self.lit, params.glow.is_some(), width, height)
            && self.halo.fit(params.glow.is_some(), width, height)
            && fit_stage(&mut self.out, params.fuzz.is_some(), width, height)
            && self.trail.fit(params.phosphor.is_some(), width, height)
    }
}

/// Hold `stage` at `width` × `height` when `needed`, and drop it otherwise.
fn fit_stage(stage: &mut Option<Surface>, needed: bool, width: u32, height: u32) -> bool {
    if !needed {
        *stage = None;
        return true;
    }
    if stage
        .as_ref()
        .is_some_and(|held| (held.width(), held.height()) == (width, height))
    {
        return true;
    }
    *stage = Surface::new(width, height);
    stage.is_some()
}

/// The pixels of a `width` × `height` screen, or `None` past an address.
fn pixel_count(width: u32, height: u32) -> Option<usize> {
    usize::try_from(width)
        .ok()?
        .checked_mul(usize::try_from(height).ok()?)
}

/// Size `buffer` to `count` items of `fill`, keeping it when it is that size
/// already; `false` when the process cannot afford it.
fn refit<T: Copy>(buffer: &mut Vec<T>, count: usize, fill: T) -> bool {
    if buffer.len() == count {
        return true;
    }
    buffer.clear();
    if buffer.try_reserve_exact(count).is_err() {
        return false;
    }
    buffer.resize(count, fill);
    true
}

/// The rows of `rect`, as `u32` surface rows.
fn rows_of(rect: Rect) -> core::ops::Range<u32> {
    let top = u32::try_from(rect.top()).unwrap_or(0);
    top..top.saturating_add(rect.height)
}

/// The columns of `rect`, as surface indices.
fn columns_of(rect: Rect) -> core::ops::Range<usize> {
    let left = usize::try_from(rect.left()).unwrap_or(0);
    left..left.saturating_add(usize::try_from(rect.width).unwrap_or(0))
}

/// The first column of `rect`, which every rectangle here holds inside the
/// screen.
fn left_of(rect: Rect) -> u32 {
    u32::try_from(rect.left()).unwrap_or(0)
}

/// The number of steps in one full turn of [`sine`].
const SINE_PERIOD: u32 = 64;

/// The amplitude [`sine`] returns at its peak.
const SINE_SCALE: i32 = 256;

/// One quarter turn of a sine, scaled to [`SINE_SCALE`]: an integer table, so
/// the wave is exactly reproducible on every target.
const SINE_QUARTER: [i32; 17] = [
    0, 25, 50, 74, 98, 121, 142, 162, 181, 198, 213, 226, 237, 245, 251, 255, 256,
];

/// The sine of `step` sixty-fourths of a turn, scaled to [`SINE_SCALE`].
fn sine(step: u32) -> i32 {
    let step = step % SINE_PERIOD;
    let within = usize::try_from(step % 16).unwrap_or(0);
    let value = |index: usize| SINE_QUARTER.get(index).copied().unwrap_or(0);
    match step / 16 {
        0 => value(within),
        1 => value(16 - within),
        2 => -value(within),
        _ => -value(16 - within),
    }
}

/// How far the wobble displaces row `y` at `phase`, in physical pixels.
///
/// A slow vertical travel plus a slow advance in time, so the wave moves down
/// the screen rather than shearing it all at once.
fn wobble_offset(y: u32, amplitude: u32, phase: Phase) -> i32 {
    let angle = y.wrapping_mul(6).wrapping_add(phase.0.wrapping_mul(3)) % SINE_PERIOD;
    sine(angle) * i32::try_from(amplitude).unwrap_or(i32::MAX) / SINE_SCALE
}

/// Move every row to its displacement at `phase`, adding each row that moved
/// to `moved` as a whole row.
fn shift_rows(offsets: &mut [i32], amplitude: u32, phase: Phase, width: u32, moved: &mut Region) {
    let mut run: Option<(u32, u32)> = None;
    for (y, offset) in (0u32..).zip(offsets.iter_mut()) {
        let next = wobble_offset(y, amplitude, phase);
        if next != *offset {
            *offset = next;
            run = Some(run.map_or((y, y + 1), |(top, _)| (top, y + 1)));
            continue;
        }
        if let Some((top, bottom)) = run.take() {
            moved.add(Rect::new(0, to_i32(top), width, bottom - top));
        }
    }
    if let Some((top, bottom)) = run {
        moved.add(Rect::new(0, to_i32(top), width, bottom - top));
    }
}

/// Add where a change to the screen inside `damage` shows in `pre` within
/// `bounds`: each [`HULL_ROWS`] rows of it as one rectangle spanning every
/// displacement those rows are carried along by.
fn displace_damage(shaded: &mut Region, damage: Rect, offsets: &[i32], bounds: Rect) {
    if damage.is_empty() {
        return;
    }
    if offsets.is_empty() {
        shaded.add(damage);
        return;
    }
    let offset_of = |y: u32| {
        usize::try_from(y)
            .ok()
            .and_then(|row| offsets.get(row).copied())
            .unwrap_or(0)
    };
    let rows = rows_of(damage);
    let mut top = rows.start;
    while top < rows.end {
        let bottom = top.saturating_add(HULL_ROWS).min(rows.end);
        let (low, high) = (top..bottom)
            .map(offset_of)
            .fold((i32::MAX, i32::MIN), |(low, high), offset| {
                (low.min(offset), high.max(offset))
            });
        let spread = high.abs_diff(low);
        shaded.add(
            Rect::new(
                damage.left().saturating_add(low),
                to_i32(top),
                damage.width.saturating_add(spread),
                bottom - top,
            )
            .intersection(&bounds),
        );
        top = bottom;
    }
}

/// Fill `dst`, the columns from `left` of a row displaced by `offset`, from
/// `src`, the screen's row: a column with nothing to its source side draws
/// transparent.
fn displace(dst: &mut [Pixel], src: &[Pixel], left: usize, offset: i32) {
    let offset = i64::from(offset);
    let len = i64::try_from(dst.len()).unwrap_or(i64::MAX);
    let first = i64::try_from(left).unwrap_or(i64::MAX);
    let row = i64::try_from(src.len()).unwrap_or(i64::MAX);
    // The columns whose source `x - offset` lies inside the screen's row.
    let start = (offset - first).clamp(0, len);
    let end = (row + offset - first).clamp(start, len);
    let to_index = |value: i64| usize::try_from(value).unwrap_or(0);
    let (start, end) = (to_index(start), to_index(end));
    let from = to_index(first + i64::try_from(start).unwrap_or(0) - offset);
    let (head, rest) = dst.split_at_mut(start);
    let (body, tail) = rest.split_at_mut(end - start);
    head.fill(Pixel::TRANSPARENT);
    if let Some(source) = src.get(from..from + body.len()) {
        body.copy_from_slice(source);
    }
    tail.fill(Pixel::TRANSPARENT);
}

/// What the `pre` stage reads beyond the screen.
struct Shading<'a> {
    /// Each row's wobble displacement; empty with no wobble.
    offsets: &'a [i32],
    /// The phosphor trail, row-major; `None` with no phosphor.
    trail: Option<&'a [u8]>,
    /// What a scan line keeps of an odd row.
    scan: Option<u8>,
}

/// Redraw `pre` over `region` from `screen`.
fn shade(
    pre: &mut Surface,
    screen: &Surface,
    region: &Region,
    shading: &Shading<'_>,
    runner: &dyn JobRunner,
) {
    let width = usize::try_from(screen.width()).unwrap_or(0);
    for &rect in region.rects() {
        let columns = columns_of(rect);
        let rows = rows_of(rect);
        let per = band_rows(runner, rect);
        for_each_drawn(runner, pre.row_bands_mut(rows, per), &|mut band| {
            for y in band.rows() {
                let (Some((_, src)), Some((_, dst))) = (
                    screen.row_span(y, 0, screen.width()),
                    band.row_span_mut(y, left_of(rect), rect.width),
                ) else {
                    continue;
                };
                let row = usize::try_from(y).unwrap_or(0);
                let offset = shading.offsets.get(row).copied().unwrap_or(0);
                displace(dst, src, columns.start, offset);
                if let Some(trail) = shading.trail {
                    let base = row.saturating_mul(width);
                    let levels = trail
                        .get(base + columns.start..base + columns.end)
                        .unwrap_or_default();
                    for (pixel, &level) in dst.iter_mut().zip(levels) {
                        if level > 0 {
                            *pixel = add_trail(*pixel, level);
                        }
                    }
                }
                if let Some(keep) = shading.scan.filter(|_| y % 2 == 1) {
                    for pixel in dst.iter_mut() {
                        *pixel = pixel.dimmed(keep);
                    }
                }
            }
        });
    }
}

/// How many rows each band of a pass over `rect` takes.
fn band_rows(runner: &dyn JobRunner, rect: Rect) -> u32 {
    let area =
        usize::try_from(u64::from(rect.width) * u64::from(rect.height)).unwrap_or(usize::MAX);
    let pieces = tairix_parallel::bands(runner, area, MIN_PARALLEL_PX);
    let rows = usize::try_from(rect.height).unwrap_or(0);
    u32::try_from(tairix_parallel::piece_len(rows, pieces)).unwrap_or(u32::MAX)
}

/// Add `level` back into `pixel`, saturating.
///
/// The trail keeps the pixel's own hue rather than washing towards white, as
/// a single-phosphor tube does, and alpha rises with it: light the tube emits
/// is not see-through, however translucent the background behind it is.
fn add_trail(pixel: Pixel, level: u8) -> Pixel {
    let lift = u8::try_from(u32::from(level) / TRAIL_SHARE).unwrap_or(u8::MAX);
    Pixel {
        r: pixel.r.saturating_add(lift),
        g: pixel.g.saturating_add(lift),
        b: pixel.b.saturating_add(lift),
        a: pixel.a.saturating_add(lift),
    }
}

/// The phosphor: how brightly each pixel was lit, decaying towards dark, and
/// which pixels a step must still visit.
#[derive(Debug, Default)]
struct Trail {
    /// Per-pixel remembered luminance, row-major.
    levels: Vec<u8>,
    /// Pixels whose level is above what their own light holds it at, so the
    /// next step lowers it.
    active: Region,
    /// Pixels whose light changed since the last step.
    pending: Region,
    /// A step's hulls of what it changed and what it left active, per
    /// [`HULL_ROWS`] rows.
    hulls: Vec<Hull>,
    /// The region a step visits, built from the two above.
    visit: Region,
}

/// The columns a step changed and left active within one group of rows.
#[derive(Copy, Clone, Debug, Default)]
struct Hull {
    changed: Span,
    active: Span,
}

/// A half-open run of columns; empty while `start >= end`.
#[derive(Copy, Clone, Debug)]
struct Span {
    start: usize,
    end: usize,
}

impl Default for Span {
    fn default() -> Self {
        Self {
            start: usize::MAX,
            end: 0,
        }
    }
}

impl Span {
    fn include(&mut self, column: usize) {
        self.start = self.start.min(column);
        self.end = self.end.max(column + 1);
    }

    fn rect(self, top: u32, rows: u32) -> Option<Rect> {
        (self.start < self.end).then(|| {
            Rect::new(
                i32::try_from(self.start).unwrap_or(i32::MAX),
                to_i32(top),
                u32::try_from(self.end - self.start).unwrap_or(u32::MAX),
                rows,
            )
        })
    }
}

impl Trail {
    fn unsettled(&self) -> bool {
        !self.active.is_empty() || !self.pending.is_empty()
    }

    /// Hold a level per pixel when `on`, starting dark with every pixel
    /// pending; let it go otherwise.
    fn fit(&mut self, on: bool, width: u32, height: u32) -> bool {
        if !on {
            *self = Self::default();
            return true;
        }
        let Some(count) = pixel_count(width, height) else {
            return false;
        };
        if self.levels.len() == count {
            return true;
        }
        if !refit(&mut self.levels, count, 0) {
            return false;
        }
        self.active.clear();
        self.pending.clear();
        self.pending.add(Rect::new(0, 0, width, height));
        true
    }

    /// One step of decay over every active or pending pixel of `screen` as
    /// the wobble displaces it, keeping `persistence` permille of each lit
    /// pixel; every pixel whose level moved is added to `changed`.
    fn step(&mut self, screen: &Surface, offsets: &[i32], persistence: u32, changed: &mut Region) {
        let width = usize::try_from(screen.width()).unwrap_or(0);
        let mut visit = mem::take(&mut self.visit);
        visit.clone_from(&self.active);
        for rect in self.pending.rects() {
            visit.add(*rect);
        }
        // A rectangle reaching past the screen would walk a row's levels into
        // the next row's.
        visit.clip(Rect::new(0, 0, screen.width(), screen.height()));
        self.pending.clear();
        self.active.clear();
        for &rect in visit.rects() {
            let rows = rows_of(rect);
            let groups = usize::try_from(rect.height.div_ceil(HULL_ROWS)).unwrap_or(0);
            self.hulls.clear();
            if self.hulls.try_reserve(groups).is_err() {
                // Without room to say what moved, report the whole rectangle
                // and keep it active: the next step visits it again.
                changed.add(rect);
                self.active.add(rect);
                continue;
            }
            self.hulls.resize(groups, Hull::default());
            let columns = columns_of(rect);
            for y in rows.clone() {
                let Some((_, src)) = screen.row_span(y, 0, screen.width()) else {
                    continue;
                };
                let row = usize::try_from(y).unwrap_or(0);
                let offset = i64::from(offsets.get(row).copied().unwrap_or(0));
                let group = usize::try_from((y - rows.start) / HULL_ROWS).unwrap_or(0);
                let Some(hull) = self.hulls.get_mut(group) else {
                    continue;
                };
                let base = row.saturating_mul(width);
                let Some(levels) = self
                    .levels
                    .get_mut(base + columns.start..base + columns.end)
                else {
                    continue;
                };
                for (column, slot) in columns.clone().zip(levels.iter_mut()) {
                    let source = i64::try_from(column).unwrap_or(i64::MAX) - offset;
                    let lit = usize::try_from(source)
                        .ok()
                        .and_then(|index| src.get(index))
                        .map_or(0, |pixel| pixel.luma());
                    let held = u32::from(lit) * persistence / u32::from(FULL);
                    let decayed = u32::from(*slot) * persistence / u32::from(FULL);
                    let next = u8::try_from(decayed.max(held)).unwrap_or(u8::MAX);
                    if next != *slot {
                        *slot = next;
                        hull.changed.include(column);
                    }
                    if u32::from(next) != held {
                        hull.active.include(column);
                    }
                }
            }
            for (group, hull) in (0u32..).zip(&self.hulls) {
                let first = rows.start + group * HULL_ROWS;
                let count = HULL_ROWS.min(rows.end - first);
                if let Some(moved) = hull.changed.rect(first, count) {
                    changed.add(moved);
                }
                if let Some(still) = hull.active.rect(first, count) {
                    self.active.add(still);
                }
            }
        }
        self.visit = visit;
    }
}

/// The glow's working buffers, each the size of the screen: a whole render
/// blurs all of it, and any neighbourhood fits inside.
#[derive(Debug, Default)]
struct Halo {
    highlights: Vec<Pixel>,
    aux: Vec<Pixel>,
}

impl Halo {
    /// Hold room to blur a `width` × `height` screen when `on`, and nothing
    /// otherwise; `false` when the process cannot afford it.
    fn fit(&mut self, on: bool, width: u32, height: u32) -> bool {
        if !on {
            *self = Self::default();
            return true;
        }
        pixel_count(width, height).is_some_and(|count| {
            refit(&mut self.highlights, count, Pixel::TRANSPARENT)
                && refit(&mut self.aux, count, Pixel::TRANSPARENT)
        })
    }

    /// Redraw `lit` over `region` from `source`, each rectangle blurred from
    /// the neighbourhood its reach spans.
    ///
    /// The tent's two boxes each read their radius around a pixel, so a
    /// pixel's glow depends on `source` within the reach of it and nowhere
    /// else: blurring the rectangle grown by the reach, clipped to the screen
    /// where the whole-screen blur replicates its edge too, writes exactly
    /// what blurring the whole screen would.
    fn glow(&mut self, lit: &mut Surface, source: &Surface, region: &Region, glow: Glow) {
        let bounds = Rect::new(0, 0, source.width(), source.height());
        let near = usize::try_from(glow.reach / 2).unwrap_or(0);
        let far = usize::try_from(glow.reach - glow.reach / 2).unwrap_or(0);
        for &rect in region.rects() {
            let read = rect.outset(glow.reach).intersection(&bounds);
            let cols = usize::try_from(read.width).unwrap_or(0);
            let rows = usize::try_from(read.height).unwrap_or(0);
            let held = cols.checked_mul(rows).and_then(|count| {
                Some((
                    self.highlights.get_mut(..count)?,
                    self.aux.get_mut(..count)?,
                ))
            });
            let Some((field, aux)) = held else {
                // The text under the glow still changed: show it unlit rather
                // than leave the old picture standing.
                copy_rect(lit, source, rect);
                continue;
            };
            for (line, y) in field.chunks_exact_mut(cols).zip(rows_of(read)) {
                let Some((_, src)) = source.row_span(y, left_of(read), read.width) else {
                    continue;
                };
                for (slot, pixel) in line.iter_mut().zip(src) {
                    *slot = highlight(*pixel);
                }
            }
            box_blur(field, cols, rows, near, aux);
            box_blur(field, cols, rows, far, aux);
            let skip_x = usize::try_from(rect.left() - read.left()).unwrap_or(0);
            for y in rows_of(rect) {
                let line = usize::try_from(y - rows_of(read).start).unwrap_or(0);
                let width = usize::try_from(rect.width).unwrap_or(0);
                let light = field
                    .get(line * cols + skip_x..)
                    .and_then(|rest| rest.get(..width))
                    .unwrap_or_default();
                let (Some((_, src)), Some((_, dst))) = (
                    source.row_span(y, left_of(rect), rect.width),
                    lit.row_span_mut(y, left_of(rect), rect.width),
                ) else {
                    continue;
                };
                for ((slot, pixel), light) in dst.iter_mut().zip(src).zip(light) {
                    *slot = add_light(*pixel, gain(*light, glow.intensity));
                }
            }
        }
    }
}

/// Copy `rect` of `source` into `dst` unchanged.
fn copy_rect(dst: &mut Surface, source: &Surface, rect: Rect) {
    for y in rows_of(rect) {
        if let (Some((_, src)), Some((_, out))) = (
            source.row_span(y, left_of(rect), rect.width),
            dst.row_span_mut(y, left_of(rect), rect.width),
        ) {
            out.copy_from_slice(src);
        }
    }
}

/// The light `pixel` spreads: how far its hardest-driven channel clears
/// [`GLOW_KNEE`], in the pixel's own colour.
///
/// The drive is the peak channel rather than a luma weighting, because a
/// phosphor driven to full emits as much light whatever its colour.
fn highlight(pixel: Pixel) -> Pixel {
    let peak = pixel.r.max(pixel.g).max(pixel.b);
    let Some(over) = peak.checked_sub(GLOW_KNEE) else {
        return Pixel::TRANSPARENT;
    };
    let span = u32::from(u8::MAX - GLOW_KNEE);
    let share = u8::try_from(u32::from(over) * u32::from(u8::MAX) / span).unwrap_or(u8::MAX);
    pixel.scale_alpha(share)
}

/// `light` scaled by `permille`, saturating each channel.
///
/// A gain, not a fade: above unity the glow adds back more light than it
/// spread. One factor and one ceiling for every channel keeps the pixel
/// premultiplied.
fn gain(light: Pixel, permille: u32) -> Pixel {
    let channel = |value: u8| {
        u8::try_from(u32::from(value).saturating_mul(permille) / u32::from(FULL)).unwrap_or(u8::MAX)
    };
    Pixel {
        r: channel(light.r),
        g: channel(light.g),
        b: channel(light.b),
        a: channel(light.a),
    }
}

/// Add `light` into `pixel`, saturating; alpha rises with the colour, as the
/// trail's does.
fn add_light(pixel: Pixel, light: Pixel) -> Pixel {
    Pixel {
        r: pixel.r.saturating_add(light.r),
        g: pixel.g.saturating_add(light.g),
        b: pixel.b.saturating_add(light.b),
        a: pixel.a.saturating_add(light.a),
    }
}

/// Pixels whose jitter one 64-bit draw supplies: a byte each.
const DRAW_PIXELS: usize = 8;

/// Redraw `out` over `region` from `source`, jittering each pixel's colour by
/// up to `amplitude` at `phase`.
///
/// The jitter of a pixel is a pure function of its position and the step, so
/// any part of the screen can be redrawn alone and lands exactly where the
/// whole-screen pass would have put it.
fn fuzz(
    out: &mut Surface,
    source: &Surface,
    region: &Region,
    amplitude: u32,
    phase: Phase,
    runner: &dyn JobRunner,
) {
    let span = amplitude.saturating_mul(2).saturating_add(1);
    let amplitude = i32::try_from(amplitude).unwrap_or(i32::MAX);
    for &rect in region.rects() {
        let columns = columns_of(rect);
        let per = band_rows(runner, rect);
        for_each_drawn(
            runner,
            out.row_bands_mut(rows_of(rect), per),
            &|mut band| {
                for y in band.rows() {
                    let (Some((_, src)), Some((_, dst))) = (
                        source.row_span(y, left_of(rect), rect.width),
                        band.row_span_mut(y, left_of(rect), rect.width),
                    ) else {
                        continue;
                    };
                    let key = (u64::from(phase.0) << 32) | u64::from(y);
                    jitter_row(dst, src, columns.start, key, span, amplitude);
                }
            },
        );
    }
}

/// Jitter `src`, the row's columns from `left`, into `dst`.
///
/// One 64-bit draw supplies eight pixels a byte each, scaled onto the span by
/// a multiply and a shift rather than a divide.
fn jitter_row(dst: &mut [Pixel], src: &[Pixel], left: usize, key: u64, span: u32, amplitude: i32) {
    let draw = |index: usize| {
        let index = u64::try_from(index).unwrap_or(u64::MAX);
        mix64(key.wrapping_add(index.wrapping_add(1).wrapping_mul(GOLDEN_GAMMA))).to_le_bytes()
    };
    let mut index = left / DRAW_PIXELS;
    let mut bytes = draw(index);
    for ((column, slot), pixel) in (left..).zip(dst.iter_mut()).zip(src) {
        if column / DRAW_PIXELS != index {
            index = column / DRAW_PIXELS;
            bytes = draw(index);
        }
        let byte = bytes.get(column % DRAW_PIXELS).copied().unwrap_or(0);
        let scaled = (u32::from(byte) * span) >> 8;
        let delta = i32::try_from(scaled).unwrap_or(0) - amplitude;
        *slot = jitter(*pixel, delta);
    }
}

/// Shift a premultiplied pixel's colour channels by `delta`, clamped to the
/// alpha it is premultiplied against so the pixel stays valid.
fn jitter(pixel: Pixel, delta: i32) -> Pixel {
    let shift = |channel: u8| -> u8 {
        let value = i32::from(channel) + delta;
        u8::try_from(value.clamp(0, i32::from(pixel.a))).unwrap_or(0)
    };
    Pixel {
        r: shift(pixel.r),
        g: shift(pixel.g),
        b: shift(pixel.b),
        a: pixel.a,
    }
}

#[cfg(test)]
#[path = "pipeline_tests.rs"]
mod tests;
