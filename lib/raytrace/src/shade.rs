//! The shade a wood's crowns cast over the ground: how far under a crown a
//! place lies, which is where the leaves and needles fall, and how much of
//! the sky the crowns about it hide, which is how little light reaches the
//! floor there.
//!
//! A lone tree hides a small share of the sky from the ground beneath it, so
//! grass grows there; a closed wood hides nearly all of it, so none does.
//! The share hidden is the share of the ground nearby that crowns cover,
//! which is what the sky a place sees is roofed by.
//!
//! A shade is cast a bounded unit at a time across a runner, since a forest
//! holds tens of thousands of crowns: they are sorted into the bands of rows
//! their trunks stand in, each band takes its cover from the crowns that can
//! reach it, and the sky hidden is spread from the cover a band at a time.

use alloc::vec::Vec;

use tairix_parallel::JobRunner;
use tairix_util::{fallible, mathf};

use crate::noise::{cell, smoothstep};
use crate::vector::{real, share};

/// A crown: where its trunk stands, and how far it reaches from it.
pub(crate) type Crown = ((f64, f64), f64);

/// A rectangle of the ground, its least and greatest corners.
pub(crate) type Rect = ((f64, f64), (f64, f64));

/// The share of its reach at which a crown's edge begins thinning out.
const EDGE: f64 = 0.55;

/// How far about a place the crowns roofing its sky stand.
pub(crate) const ROOFED: f64 = 12.0;

/// How far either way of the eye the fine shade over a land reaches, how far
/// apart it is sampled, and the most samples a side of the coarse shade over
/// the rest of the land holds.
const NEAR: f64 = 256.0;
pub(crate) const NEAR_CELL: f64 = 1.0;
const FAR_SIDE: usize = 512;

/// Crowns one unit of a cast sorts into its bands.
const SORT_UNIT: usize = 16_384;

/// Rows of a shade's grid a band holds: a core's share of a unit.
const BAND_ROWS: usize = 32;

/// How many times a shade's cover is spread each way: twice, so each crown's
/// share is weighed by how near it stands.
const SPREADS: usize = 2;

/// A grid over a rectangle of the ground, holding at each corner of its
/// cells how far under a crown it lies and how much of the sky is hidden
/// there, each `0` to `255`; or open ground, holding nothing, where no crown
/// reaches.
#[derive(Clone, Debug)]
pub(crate) struct Shade {
    least: (f64, f64),
    cell: f64,
    columns: usize,
    rows: usize,
    /// How far under a crown each corner lies, and the sky hidden there.
    cover: Vec<[u8; 2]>,
}

impl Shade {
    /// Open ground, which no crown shades.
    const OPEN: Self = Self {
        least: (0.0, 0.0),
        cell: 1.0,
        columns: 0,
        rows: 0,
        cover: Vec::new(),
    };

    /// The shade `crowns` cast over `rect`, cast whole on the calling thread.
    #[cfg(test)]
    pub(crate) fn of(crowns: &[Crown], rect: Rect, cell: f64, spread: f64) -> Option<Self> {
        let mut casting = Casting::new(crowns, rect, cell, spread)?;
        while !casting.step(crowns, &tairix_parallel::SERIAL)? {}
        Some(casting.finish())
    }

    /// Whether no crown shades any of it.
    pub(crate) fn is_open(&self) -> bool {
        self.cover.is_empty()
    }

    /// How far under a crown `(x, z)` lies, and how much of the sky is
    /// hidden there, each `0.0..=1.0` and blended between the grid's
    /// samples; nought off the grid.
    pub(crate) fn at(&self, x: f64, z: f64) -> (f64, f64) {
        let Some((across, down)) = self.place(x, z) else {
            return (0.0, 0.0);
        };
        let ((column, right), (row, lower)) = (cell(across), cell(down));
        let (column, row) = (column as usize, row as usize);
        let sample = |column: usize, row: usize| {
            let (column, row) = (column.min(self.columns - 1), row.min(self.rows - 1));
            let [under, hidden] = self
                .cover
                .get(row * self.columns + column)
                .copied()
                .unwrap_or([0; 2]);
            (f64::from(under), f64::from(hidden))
        };
        let blend = |pick: fn((f64, f64)) -> f64| {
            let at = |column: usize, row: usize| pick(sample(column, row));
            let top = at(column, row) + (at(column + 1, row) - at(column, row)) * right;
            let bottom =
                at(column, row + 1) + (at(column + 1, row + 1) - at(column, row + 1)) * right;
            (top + (bottom - top) * lower) / 255.0
        };
        (blend(|(under, _)| under), blend(|(_, hidden)| hidden))
    }

    /// Where `(x, z)` lies on the grid, in samples along and down it; `None`
    /// off it, or over open ground.
    fn place(&self, x: f64, z: f64) -> Option<(f64, f64)> {
        if self.is_open() {
            return None;
        }
        let (across, down) = (
            (x - self.least.0) / self.cell,
            (z - self.least.1) / self.cell,
        );
        let on = (0.0..=real(self.columns - 1)).contains(&across)
            && (0.0..=real(self.rows - 1)).contains(&down);
        on.then_some((across, down))
    }

    /// A copy of this shade; `None` when the heap will not hold it.
    pub(crate) fn copied(&self) -> Option<Self> {
        Some(Self {
            cover: fallible::collected(self.cover.len(), self.cover.iter().copied())?,
            ..*self
        })
    }
}

/// How many samples `cell` apart span `span`, both ends included; `None` for
/// more than a grid can count.
fn samples(span: f64, cell: f64) -> Option<usize> {
    usize::try_from(mathf::round_i32(mathf::ceil(span / cell)).max(1))
        .ok()
        .map(|count| count + 1)
}

/// The first sample of `most` at or below `low`, sampled `cell` apart from
/// `least`.
fn first(low: f64, least: f64, cell: f64, most: usize) -> usize {
    let at = mathf::round_i32(mathf::floor((low - least) / cell))
        .clamp(0, i32::try_from(most).unwrap_or(i32::MAX) - 1);
    usize::try_from(at).unwrap_or(0)
}

/// A shade being cast over a rectangle, a bounded unit at a time.
#[derive(Debug)]
pub(crate) struct Casting {
    least: (f64, f64),
    /// The far corner of the rectangle asked for, which the grid's last
    /// samples may lie a little beyond.
    most: (f64, f64),
    cell: f64,
    columns: usize,
    rows: usize,
    /// How many cells about a place the crowns roofing its sky stand.
    radius: usize,
    /// The crowns cast from, and how many bands either side of the one its
    /// trunk stands in a crown can reach.
    crowns: usize,
    reach: usize,
    /// Where each band's crowns begin in `sorted`, and the crowns, by index.
    starts: Vec<u32>,
    sorted: Vec<u32>,
    cover: Vec<[u8; 2]>,
    /// The cover being spread over the sky it hides, and its transpose.
    spread: [Vec<f64>; 2],
    stage: Cast,
}

/// Where a cast stands.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Cast {
    /// Counting the crowns into their bands, from crown `next`.
    Count(usize),
    /// Placing them there, from crown `next`.
    Place(usize),
    /// Taking each band's cover from its crowns, from band `next`.
    Cover(usize),
    /// Spreading the cover over the sky it hides, pass `pass` of `PASSES`
    /// from row `next` of what that pass writes.
    Spread {
        pass: usize,
        next: usize,
    },
    Done,
}

/// The passes of a cast's spreading: the cover read in; along its rows, then
/// turned, along what were its columns, and turned back, `SPREADS` times; and
/// the sky hidden written out.
const PASSES: usize = 2 + 4 * SPREADS;

impl Casting {
    /// The shade `crowns` will cast over `rect`, sampled `cell` apart, the sky
    /// a place sees roofed by the crowns within `spread` of it; `None` when
    /// the heap will not hold its grid.
    pub(crate) fn new(crowns: &[Crown], (from, to): Rect, cell: f64, spread: f64) -> Option<Self> {
        let cell = cell.max(1e-3);
        let (columns, rows) = (samples(to.0 - from.0, cell)?, samples(to.1 - from.1, cell)?);
        let bands = rows.div_ceil(BAND_ROWS);
        Some(Self {
            least: from,
            most: to,
            cell,
            columns,
            rows,
            radius: usize::try_from(mathf::round_i32(spread / cell).max(1)).ok()?,
            crowns: crowns.len(),
            reach: 0,
            starts: fallible::filled(bands + 1, 0u32)?,
            sorted: Vec::new(),
            cover: Vec::new(),
            spread: [Vec::new(), Vec::new()],
            stage: Cast::Count(0),
        })
    }

    /// The band of rows crown `crown`'s trunk stands in, if its crown reaches
    /// over the rectangle at all.
    fn band_of(&self, &((x, z), reach): &Crown) -> Option<usize> {
        let (from, to) = (self.least, self.most);
        let reach = reach.max(1e-3);
        let over =
            x + reach >= from.0 && x - reach <= to.0 && z + reach >= from.1 && z - reach <= to.1;
        over.then(|| first(z, from.1, self.cell, self.rows) / BAND_ROWS)
    }

    /// How far the cast has come.
    pub(crate) fn done(&self) -> f64 {
        let sorting = |next: usize| share(next, self.crowns);
        match self.stage {
            Cast::Count(next) => 0.1 * sorting(next),
            Cast::Place(next) => 0.1 + 0.1 * sorting(next),
            Cast::Cover(next) => 0.2 + 0.5 * share(next, self.starts.len().saturating_sub(1)),
            Cast::Spread { pass, next } => {
                let rows = self.written_by(pass);
                0.7 + 0.3 * (real(pass) + share(next, rows)) / real(PASSES)
            }
            Cast::Done => 1.0,
        }
    }

    /// Cast the next unit across `runner` from `crowns`, the crowns it was
    /// begun with: whether the shade is cast, or `None` when the heap refused
    /// it.
    pub(crate) fn step(&mut self, crowns: &[Crown], runner: &dyn JobRunner) -> Option<bool> {
        let crowns = crowns.get(..self.crowns)?;
        let width = runner.width().max(1);
        self.stage = match self.stage {
            Cast::Count(next) => self.count(crowns, next)?,
            Cast::Place(next) => self.place(crowns, next),
            Cast::Cover(next) => self.take_cover(crowns, next, width, runner)?,
            Cast::Spread { pass, next } => self.spreading((pass, next), width, runner)?,
            Cast::Done => Cast::Done,
        };
        Some(self.stage == Cast::Done)
    }

    /// The shade cast.
    pub(crate) fn finish(self) -> Shade {
        if self.cover.is_empty() {
            return Shade::OPEN;
        }
        Shade {
            least: self.least,
            cell: self.cell,
            columns: self.columns,
            rows: self.rows,
            cover: self.cover,
        }
    }

    /// A unit of the crowns from `next` counted into their bands, and how
    /// far each band's crowns can reach; the bands laid end to end once all
    /// are, and open ground if none reaches.
    fn count(&mut self, crowns: &[Crown], next: usize) -> Option<Cast> {
        let end = (next + SORT_UNIT).min(crowns.len());
        let mut farthest = 0.0f64;
        for crown in crowns.get(next..end)? {
            if let Some(band) = self.band_of(crown) {
                *self.starts.get_mut(band + 1)? += 1;
                farthest = farthest.max(crown.1);
            }
        }
        let rows = usize::try_from(mathf::round_i32(mathf::ceil(farthest / self.cell))).ok()?;
        self.reach = self.reach.max((rows + 1).div_ceil(BAND_ROWS) + 1);
        if end < crowns.len() {
            return Some(Cast::Count(end));
        }
        for band in 1..self.starts.len() {
            let before = *self.starts.get(band - 1)?;
            *self.starts.get_mut(band)? += before;
        }
        let total = self.starts.last().copied().unwrap_or(0) as usize;
        if total == 0 {
            return Some(Cast::Done);
        }
        self.sorted = fallible::filled(total, 0u32)?;
        self.cover = fallible::filled(self.columns * self.rows, [0u8; 2])?;
        Some(Cast::Place(0))
    }

    /// A unit of the crowns from `next` placed in their bands: each band's
    /// start moves on past the crowns placed in it, and once all are, each
    /// start stands where the band before it ended, so shifts back one.
    fn place(&mut self, crowns: &[Crown], next: usize) -> Cast {
        let end = (next + SORT_UNIT).min(crowns.len());
        for (index, crown) in (next..end).zip(crowns.get(next..end).unwrap_or_default()) {
            let Some(band) = self.band_of(crown) else {
                continue;
            };
            if let Some(start) = self.starts.get_mut(band) {
                if let Some(slot) = self.sorted.get_mut(*start as usize) {
                    *slot = u32::try_from(index).unwrap_or(u32::MAX);
                }
                *start += 1;
            }
        }
        if end < crowns.len() {
            return Cast::Place(end);
        }
        self.starts.rotate_right(1);
        if let Some(first) = self.starts.first_mut() {
            *first = 0;
        }
        Cast::Cover(0)
    }

    /// A unit of the bands from `next`, `width` of them across `runner`, each
    /// taking its cover from the crowns that can reach it.
    fn take_cover(
        &mut self,
        crowns: &[Crown],
        next: usize,
        width: usize,
        runner: &dyn JobRunner,
    ) -> Option<Cast> {
        let bands = self.starts.len() - 1;
        let end = (next + width).min(bands);
        let (columns, rows, cell, least, reach) =
            (self.columns, self.rows, self.cell, self.least, self.reach);
        let (starts, sorted) = (&self.starts, &self.sorted);
        let span = next * BAND_ROWS * columns..(end * BAND_ROWS).min(rows) * columns;
        let cover = self.cover.get_mut(span)?;
        crate::band::for_each(
            runner,
            cover,
            (next, BAND_ROWS * columns),
            &|band, cover| {
                let top = band * BAND_ROWS;
                let bottom = top + cover.len() / columns;
                let near = band.saturating_sub(reach)..(band + reach + 1).min(bands);
                let (Some(&from), Some(&to)) = (starts.get(near.start), starts.get(near.end))
                else {
                    return;
                };
                for &index in sorted.get(from as usize..to as usize).unwrap_or_default() {
                    let Some(&((x, z), reach)) = crowns.get(index as usize) else {
                        continue;
                    };
                    let reach = reach.max(1e-3);
                    let (west, east) = (
                        first(x - reach, least.0, cell, columns),
                        first(x + reach, least.0, cell, columns) + 1,
                    );
                    let (south, north) = (
                        first(z - reach, least.1, cell, rows),
                        first(z + reach, least.1, cell, rows) + 1,
                    );
                    for row in south.max(top)..=north.min(bottom - 1) {
                        let dz = least.1 + real(row) * cell - z;
                        for column in west..=east.min(columns - 1) {
                            let dx = least.0 + real(column) * cell - x;
                            let over = 1.0 - smoothstep(EDGE * reach, reach, mathf::hypot(dx, dz));
                            if let Some([under, _]) = cover.get_mut((row - top) * columns + column)
                            {
                                *under = (*under).max(byte(over));
                            }
                        }
                    }
                }
            },
        );
        if end < bands {
            return Some(Cast::Cover(end));
        }
        // Reserved whole, but written a unit at a time by the passes that
        // first fill each.
        for buffer in &mut self.spread {
            if !fallible::reserve(buffer, columns * rows) {
                return None;
            }
        }
        Some(Cast::Spread { pass: 0, next: 0 })
    }

    /// How many rows pass `pass` of the spreading writes: the grid's own, or
    /// across its columns while it lies turned.
    fn written_by(&self, pass: usize) -> usize {
        if pass % 4 == 2 || pass % 4 == 3 {
            self.columns
        } else {
            self.rows
        }
    }

    /// A unit of spreading pass `pass` from row `next`, `width` bands of it
    /// across `runner`; the next pass begun once this one is done.
    fn spreading(
        &mut self,
        (pass, next): (usize, usize),
        width: usize,
        runner: &dyn JobRunner,
    ) -> Option<Cast> {
        let rows = self.written_by(pass);
        let end = (next + width * BAND_ROWS).min(rows);
        let (columns, height, radius) = (self.columns, self.rows, self.radius);
        let [grid, turned] = &mut self.spread;
        let ok = match pass {
            0 => {
                grid.resize(end * columns, 0.0);
                let read = self.cover.get(next * columns..end * columns)?;
                let written = grid.get_mut(next * columns..end * columns)?;
                for (value, [under, _]) in written.iter_mut().zip(read) {
                    *value = f64::from(*under);
                }
                true
            }
            last if last == PASSES - 1 => {
                let read = grid.get(next * columns..end * columns)?;
                let written = self.cover.get_mut(next * columns..end * columns)?;
                for ([_, hidden], &value) in written.iter_mut().zip(read) {
                    *hidden = u8::try_from(mathf::round_i32(value).clamp(0, 255)).unwrap_or(0);
                }
                true
            }
            // Along the rows of the grid as it lies, then of it turned.
            pass if pass % 4 == 1 => smooth_rows(grid, (columns, next..end), radius, runner),
            pass if pass % 4 == 3 => smooth_rows(turned, (height, next..end), radius, runner),
            pass if pass % 4 == 2 => {
                if turned.len() < end * height {
                    turned.resize(end * height, 0.0);
                }
                turn(grid, turned, (columns, height), next..end, runner);
                true
            }
            _ => {
                turn(turned, grid, (height, columns), next..end, runner);
                true
            }
        };
        if !ok {
            return None;
        }
        Some(if end < rows {
            Cast::Spread { pass, next: end }
        } else if pass + 1 < PASSES {
            Cast::Spread {
                pass: pass + 1,
                next: 0,
            }
        } else {
            self.spread = [Vec::new(), Vec::new()];
            Cast::Done
        })
    }
}

/// Rows `rows` of `grid`, each `length` long, each value replaced by the
/// mean of those within `radius` of it along its row, a band of rows a core
/// across `runner`; whether the heap held each band's running sums.
fn smooth_rows(
    grid: &mut [f64],
    (length, rows): (usize, core::ops::Range<usize>),
    radius: usize,
    runner: &dyn JobRunner,
) -> bool {
    let Some(band) = grid.get_mut(rows.start * length..rows.end * length) else {
        return false;
    };
    crate::band::fold(
        runner,
        band,
        (0, BAND_ROWS * length),
        true,
        &|_, band| {
            let Some(mut sums) = fallible::filled(length + 1, 0.0f64) else {
                return false;
            };
            for run in band.chunks_mut(length) {
                smooth(run, 1, radius, &mut sums);
            }
            true
        },
        |held, also| held && also,
    )
}

/// Rows `rows` of `to`, `from` turned on its side: row `r` of `to` is column
/// `r` of `from`, a grid `columns` wide and `rows_of` high, a band of rows a
/// core across `runner`.
fn turn(
    from: &[f64],
    to: &mut [f64],
    (columns, rows_of): (usize, usize),
    rows: core::ops::Range<usize>,
    runner: &dyn JobRunner,
) {
    let Some(written) = to.get_mut(rows.start * rows_of..rows.end * rows_of) else {
        return;
    };
    crate::band::for_each(
        runner,
        written,
        (rows.start / BAND_ROWS, BAND_ROWS * rows_of),
        &|band, written| {
            let first = band * BAND_ROWS;
            for (column, out) in (first..).zip(written.chunks_mut(rows_of)) {
                for (row, slot) in out.iter_mut().enumerate() {
                    *slot = from.get(row * columns + column).copied().unwrap_or(0.0);
                }
            }
        },
    );
}

/// The shade a lawn reads over its rectangle, sampled from the shade over
/// the land a bounded unit at a time.
#[derive(Debug)]
pub(crate) struct Sampling {
    least: (f64, f64),
    cell: f64,
    columns: usize,
    rows: usize,
    cover: Vec<[u8; 2]>,
    next: usize,
    touched: bool,
}

impl Sampling {
    /// The shade over `rect` to be sampled `cell` apart; `None` when the heap
    /// will not hold its grid.
    pub(crate) fn new((from, to): Rect, cell: f64) -> Option<Self> {
        let cell = cell.max(1e-3);
        let (columns, rows) = (samples(to.0 - from.0, cell)?, samples(to.1 - from.1, cell)?);
        Some(Self {
            least: from,
            cell,
            columns,
            rows,
            cover: fallible::filled(columns * rows, [0u8; 2])?,
            next: 0,
            touched: false,
        })
    }

    /// How far the sampling has come.
    pub(crate) fn done(&self) -> f64 {
        share(self.next, self.rows)
    }

    /// Sample the next unit of rows of `shades` across `runner`: whether
    /// every row is sampled.
    pub(crate) fn step(&mut self, shades: &Shades, runner: &dyn JobRunner) -> bool {
        let end = (self.next + runner.width().max(1) * BAND_ROWS).min(self.rows);
        let (columns, least, cell) = (self.columns, self.least, self.cell);
        let span = self.next * columns..end * columns;
        let Some(cover) = self.cover.get_mut(span) else {
            return true;
        };
        let touched = crate::band::fold(
            runner,
            cover,
            (self.next / BAND_ROWS, BAND_ROWS * columns),
            false,
            &|band, cover| {
                let mut touched = false;
                for (row, out) in (band * BAND_ROWS..).zip(cover.chunks_mut(columns)) {
                    for (column, slot) in out.iter_mut().enumerate() {
                        let (over, roofed) =
                            shades.at(least.0 + real(column) * cell, least.1 + real(row) * cell);
                        *slot = [byte(over), byte(roofed)];
                        touched |= *slot != [0, 0];
                    }
                }
                touched
            },
            |was, also| was || also,
        );
        self.touched |= touched;
        self.next = end;
        self.next >= self.rows
    }

    /// The shade sampled: open if no crown reaches over it.
    pub(crate) fn finish(self) -> Shade {
        if !self.touched {
            return Shade::OPEN;
        }
        Shade {
            least: self.least,
            cell: self.cell,
            columns: self.columns,
            rows: self.rows,
            cover: self.cover,
        }
    }
}

/// The shade over a land: finely about the eye, where a blade's worth of it
/// shows, and coarsely over the rest.
#[derive(Clone, Debug)]
pub(crate) struct Shades {
    near: Shade,
    far: Shade,
}

impl Shades {
    /// The shade `crowns` cast over the square `reach` either way of
    /// `centre`, finely about `eye`, cast whole on the calling thread.
    #[cfg(test)]
    pub(crate) fn of(crowns: &[Crown], land: ((f64, f64), f64), eye: (f64, f64)) -> Option<Self> {
        let mut shading = Shading::new(crowns, land, eye)?;
        while !shading.step(crowns, &tairix_parallel::SERIAL)? {}
        Some(shading.finish())
    }

    /// A copy of this shade; `None` when the heap will not hold it.
    pub(crate) fn copied(&self) -> Option<Self> {
        Some(Self {
            near: self.near.copied()?,
            far: self.far.copied()?,
        })
    }

    /// How far under a crown `(x, z)` lies, and how much of the sky is
    /// hidden there, each `0.0..=1.0`.
    pub(crate) fn at(&self, x: f64, z: f64) -> (f64, f64) {
        if self.near.place(x, z).is_some() {
            self.near.at(x, z)
        } else {
            self.far.at(x, z)
        }
    }

    /// This shade over `rect` alone, sampled `cell` apart on the calling
    /// thread: open if no crown reaches over it.
    #[cfg(test)]
    pub(crate) fn within(&self, rect: Rect, cell: f64) -> Option<Shade> {
        let mut sampling = Sampling::new(rect, cell)?;
        while !sampling.step(self, &tairix_parallel::SERIAL) {}
        Some(sampling.finish())
    }
}

/// The shade over a land being cast: finely about the eye, then coarsely
/// over the rest.
#[derive(Debug)]
pub(crate) struct Shading {
    near: Casting,
    far: Casting,
}

impl Shading {
    /// The shade `crowns` will cast over the square `reach` either way of
    /// `centre`, finely about `eye`; `None` when the heap will not hold it.
    pub(crate) fn new(
        crowns: &[Crown],
        (centre, reach): ((f64, f64), f64),
        eye: (f64, f64),
    ) -> Option<Self> {
        let about = |middle: (f64, f64), reach: f64| {
            (
                (middle.0 - reach, middle.1 - reach),
                (middle.0 + reach, middle.1 + reach),
            )
        };
        let coarse = (2.0 * reach / real(FAR_SIDE)).max(NEAR_CELL);
        Some(Self {
            near: Casting::new(crowns, about(eye, NEAR), NEAR_CELL, ROOFED)?,
            far: Casting::new(crowns, about(centre, reach), coarse, ROOFED)?,
        })
    }

    /// How far the shading has come.
    pub(crate) fn done(&self) -> f64 {
        f64::midpoint(self.near.done(), self.far.done())
    }

    /// Cast the next unit across `runner` from `crowns`, the crowns it was
    /// begun with: whether both are cast, or `None` when the heap refused it.
    pub(crate) fn step(&mut self, crowns: &[Crown], runner: &dyn JobRunner) -> Option<bool> {
        if self.near.stage != Cast::Done {
            self.near.step(crowns, runner)?;
            return Some(false);
        }
        self.far.step(crowns, runner)
    }

    /// The shade over the land.
    pub(crate) fn finish(self) -> Shades {
        Shades {
            near: self.near.finish(),
            far: self.far.finish(),
        }
    }
}

/// `share`, `0.0..=1.0`, as a byte.
fn byte(share: f64) -> u8 {
    u8::try_from(mathf::round_i32(255.0 * share.clamp(0.0, 1.0))).unwrap_or(u8::MAX)
}

/// Each of the values `stride` apart along `run` replaced by the mean of
/// those within `radius` of it, the run's first and last held beyond its
/// ends; `sums` is room for the running sums.
fn smooth(run: &mut [f64], stride: usize, radius: usize, sums: &mut [f64]) {
    let stride = stride.max(1);
    let count = run.len().div_ceil(stride);
    if count == 0 || sums.len() <= count {
        return;
    }
    let value = |index: usize| run.get(index * stride).copied().unwrap_or(0.0);
    let (first, last) = (value(0), value(count - 1));
    let mut total = 0.0;
    for index in 0..=count {
        if let Some(sum) = sums.get_mut(index) {
            *sum = total;
        }
        total += value(index);
    }
    let width = real(2 * radius + 1);
    for index in 0..count {
        let (start, end) = (
            index.saturating_sub(radius),
            (index + radius + 1).min(count),
        );
        let (before, after) = (
            radius.saturating_sub(index),
            (index + radius + 1).saturating_sub(count),
        );
        let within =
            sums.get(end).copied().unwrap_or(0.0) - sums.get(start).copied().unwrap_or(0.0);
        let mean = (within + real(before) * first + real(after) * last) / width;
        if let Some(slot) = run.get_mut(index * stride) {
            *slot = mean;
        }
    }
}

#[cfg(test)]
#[path = "shade_tests.rs"]
mod tests;
