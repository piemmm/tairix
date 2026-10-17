//! Cumulus as their thermals raise them: each heap a cloud of its own.
//!
//! A heaped deck's heaps are drawn as a Poisson field over a lattice fixed to
//! the land, so the same heap stands at a place from any eye and on every
//! level, as many as cover the deck's share of the sky, gathered by its slow
//! drifting field into drifts and streets. Their breadths fall as the
//! power-law size distribution of cumulus fields has them (Neggers, Jonker and
//! Siebesma, "Size statistics of cumulus cloud populations in large-eddy
//! simulations", 2003), from a quarter of the deck's scale up to the whole of
//! it, and each stands as deep as its breadth gives it within the deck's
//! depths, on a flat base at its air's condensation level. A level holds the
//! heaps over its square and, for each of its weather cells, those any of
//! whose cloud can stand over it, so a read meets every heap exactly however
//! far off it stands. A level's heaps are drawn a band of it at a time and
//! listed a row of its cells at a time, so no unit of laying them grows with
//! its square.

use alloc::vec::Vec;
use core::f64::consts::PI;
use core::ops::Range;

use tairix_mix::{mix32, unit};
use tairix_noise::hash3;
use tairix_parallel::JobRunner;
use tairix_util::{fallible, mathf};

use super::form::{self, Placed, EMPTY_BAND, LEAST_RISE};
use super::{Cells, Deck, WEATHER_CELLS};
use crate::sample::poisson;
use crate::shade::Rect;
use crate::vector::{above, cell_of, power, real, single};

/// Decks this heaped or more stand as heaps of their own, each its own
/// thermal's; a less heaped deck lies as a field of cloud broken into
/// footprints.
const CELLULAR: f64 = 0.75;

/// A deck's heaps are no broader than its weather's scale and no narrower
/// than this share of it, their number by breadth falling as its power
/// `SIZES`.
const NARROWEST: f64 = 0.25;
const SIZES: f64 = 1.7;

/// How far a heap's depth strays either way of the depth its breadth gives
/// it, as a share of that depth.
const DEPTH_STRAY: f64 = 0.15;

/// How far the drifting field gathers heaps: where it is highest they stand
/// so much more often than on the whole, and where lowest so much less.
const GATHERING: f64 = 0.6;

/// A lattice cell heaps are drawn over is this many of the deck's broadest
/// heaps across, along the wind as their stretch draws them out, unless it
/// would then hold more than `CROWDED` heaps on the whole, when it is drawn
/// as much narrower as leaves it holding that many.
const LATTICE: f64 = 2.0;
const CROWDED: f64 = 24.0;

/// The most heaps a lattice cell holds: so far past [`CROWDED`] that no draw
/// of a cell's count reaches it.
const MOST_IN_A_CELL: u32 = 64;

/// The most of the sky heaps are drawn to cover, short of a whole sky their
/// Poisson field could cover only with endless heaps.
const MOST_COVER: f64 = 0.95;

/// The most lattice cells heaps are drawn over for one square: past it a
/// deck's scale is too fine for the square to draw.
const MOST_LATTICE_CELLS: u64 = 1 << 22;

/// What salts a deck's heaps' draws, apart from its other fields'.
const SALT: u32 = 0x4ea9_5cf1;

/// Whether `deck` stands as heaps of its own.
pub(super) fn cellular(deck: &Deck) -> bool {
    deck.heap >= CELLULAR
}

/// One heap: where its middle stands, the radius of its footprint across the
/// wind, its depth, and the height of its base.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(super) struct Heap {
    pub(super) east: f32,
    pub(super) north: f32,
    pub(super) radius: f32,
    pub(super) depth: f32,
    pub(super) base: f32,
}

/// A heap listed over a weather cell, with the furthest any place over the
/// cell can lie within it, by which a cell's heaps are listed, furthest
/// first, so a read stops at the first that cannot better what it has met.
#[derive(Copy, Clone, Debug, PartialEq)]
struct Member {
    heap: u32,
    most: f32,
}

impl Heap {
    /// Its envelope's shape: as deep as it stands, rising to that depth at its
    /// middle.
    fn shape(&self) -> (f64, f64) {
        (
            f64::from(self.depth).max(LEAST_RISE),
            f64::from(self.radius).max(LEAST_RISE),
        )
    }
}

/// One row of a level's weather cells' lists: where each cell's heaps start
/// among `members`, a last entry closing the last cell's.
#[derive(Clone, Debug, Default)]
struct Row {
    starts: Vec<u32>,
    members: Vec<Member>,
}

/// Where a deck's heaps are drawn about a level: the rectangle holding the
/// middle of every heap whose cloud can reach its square, cut from south to
/// north into [`WEATHER_CELLS`] bands `band` deep; the breadth of the lattice
/// cells they are drawn over; and how many a lattice cell holds on the whole
/// before the drifting field thins them.
#[derive(Copy, Clone, Debug, Default)]
struct Drawing {
    low: (f64, f64),
    high: (f64, f64),
    band: f64,
    lattice: f64,
    most: f64,
}

impl Drawing {
    /// `deck`'s heaps drawn over the rectangle `low..high`, east and north;
    /// `None` when it spans more lattice cells than [`MOST_LATTICE_CELLS`].
    fn over(deck: &Deck, (low, high): Rect) -> Option<Self> {
        let lattice = lattice_of(deck);
        if !(lattice > 0.0 && lattice.is_finite()) {
            return None;
        }
        let index = |value: f64| i64::from(lattice_index(value, lattice));
        let span = |from: f64, to: f64| u64::try_from(index(to) - index(from) + 1).unwrap_or(0);
        if span(low.0, high.0).saturating_mul(span(low.1, high.1)) > MOST_LATTICE_CELLS {
            return None;
        }
        Some(Self {
            low,
            high,
            band: (high.1 - low.1) / real(WEATHER_CELLS),
            lattice,
            most: (1.0 + GATHERING) * heaps_a_cell(deck, lattice),
        })
    }

    /// Where band `band`'s south edge lies, past the last band its north
    /// edge: one reckoning for both bands an edge parts, so each heap lies
    /// in exactly one.
    fn edge(&self, band: usize) -> f64 {
        if band >= WEATHER_CELLS {
            self.high.1
        } else {
            self.low.1 + real(band) * self.band
        }
    }

    /// The band whose rows `north` lies along, the rectangle's first and
    /// last taking all south and north of it.
    fn band_of(&self, north: f64) -> usize {
        cell_of((north - self.low.1) / self.band)
            .0
            .min(WEATHER_CELLS - 1)
    }

    /// `heaps` drawn afresh as `deck`'s whose middles lie in band `band`:
    /// each lattice cell's own whatever rectangle or band it is drawn for;
    /// `None` when the heap will not hold them.
    fn fill(&self, deck: &Deck, band: usize, heaps: &mut Vec<Heap>) -> Option<()> {
        heaps.clear();
        let (south, north) = (self.edge(band), self.edge(band + 1));
        let index = |value: f64| lattice_index(value, self.lattice);
        let columns = index(self.low.0)..=index(self.high.0);
        for j in index(south)..=index(north) {
            for i in columns.clone() {
                let key = hash3(i.cast_unsigned(), j.cast_unsigned(), 0, deck.seed ^ SALT);
                for drawn in 1..=poisson(self.most, unit(key), MOST_IN_A_CELL) {
                    let key = hash3(
                        i.cast_unsigned(),
                        j.cast_unsigned(),
                        drawn,
                        deck.seed ^ SALT,
                    );
                    let (east, along) = (
                        (f64::from(i) + unit(mix32(key ^ 1))) * self.lattice,
                        (f64::from(j) + unit(mix32(key ^ 2))) * self.lattice,
                    );
                    if !((self.low.0..self.high.0).contains(&east)
                        && (south..north).contains(&along))
                    {
                        continue;
                    }
                    // Thinned as the drifting field gathers them.
                    let drift = form::drift(deck, form::drifting(deck, (east, along)), 0.0);
                    let kept = (1.0 + GATHERING * drift.clamp(-1.0, 1.0)) / (1.0 + GATHERING);
                    if unit(mix32(key ^ 3)) >= kept {
                        continue;
                    }
                    let across = breadth_drawn(deck, unit(mix32(key ^ 4)));
                    heaps.try_reserve(1).ok()?;
                    heaps.push(Heap {
                        east: single(east),
                        north: single(along),
                        radius: single(0.5 * across),
                        depth: single(depth_of(deck, across, unit(mix32(key ^ 5)))),
                        base: single(form::base_at(deck, (east, along), 0.0)),
                    });
                }
            }
        }
        Some(())
    }
}

/// The lattice cell `value` lies in along an axis whose cells are `lattice`
/// across.
fn lattice_index(value: f64, lattice: f64) -> i32 {
    mathf::round_i32(mathf::floor(value / lattice))
}

/// A deck's heaps over a level's square, and for each of the level's weather
/// cells those any of whose cloud can stand over it: begun, drawn band by
/// band, gathered, and then listed row by row.
#[derive(Clone, Debug, Default)]
pub(super) struct Heaps {
    laid: Vec<Heap>,
    /// Where each band's heaps start among `laid`, a last entry closing the
    /// last band's.
    bands: Vec<u32>,
    /// Each band's heaps as they are drawn, gathered into `laid` once the
    /// last is.
    drawn: Vec<Vec<Heap>>,
    /// The lists of each row of the level's weather cells.
    rows: Vec<Row>,
    drawing: Drawing,
    /// The deck's heading, as its cosine and sine, and how many times longer
    /// than broad its heaps are drawn out along it.
    frame: (f64, f64, f64),
    /// The level's south-west corner and the breadth of its weather cells.
    corner: (f64, f64),
    size: f64,
    /// The least share of its way out past a footprint a place outside lies
    /// from the envelope.
    outside: f64,
    /// How far past its envelope a heap's cloud is listed as standing, and
    /// the furthest from its middle that takes any heap's.
    margin: f64,
    furthest: f64,
}

impl Heaps {
    /// Room for `deck`'s heaps over the square `half` either way of `centre`,
    /// each to be listed in every weather cell within `margin` of its
    /// footprint; `None` when the heap will not hold the room, or the
    /// lattice is too fine for the square to draw.
    pub(super) fn begin(
        deck: &Deck,
        (centre, half): ((f64, f64), f64),
        margin: f64,
    ) -> Option<Self> {
        let frame = frame_of(deck);
        let corner = (centre.0 - half, centre.1 - half);
        let outside = form::outside_share();
        let furthest = (0.5 * deck.scale + margin / outside) * frame.2;
        let drawing = Drawing::over(
            deck,
            (
                (corner.0 - furthest, corner.1 - furthest),
                (centre.0 + half + furthest, centre.1 + half + furthest),
            ),
        )?;
        Some(Self {
            laid: Vec::new(),
            bands: Vec::new(),
            drawn: fallible::filled(WEATHER_CELLS, Vec::new())?,
            rows: fallible::filled(WEATHER_CELLS, Row::default())?,
            drawing,
            frame,
            corner,
            size: 2.0 * half / real(WEATHER_CELLS),
            outside,
            margin,
            furthest,
        })
    }

    /// Draw `deck`'s heaps over bands `bands` across `runner`, and once the
    /// last is drawn gather them all; `None` when the heap will not hold
    /// them.
    pub(super) fn draw(
        &mut self,
        deck: &Deck,
        bands: Range<usize>,
        runner: &dyn JobRunner,
    ) -> Option<()> {
        let drawing = self.drawing;
        let held = self.drawn.get_mut(bands.clone()).is_some_and(|slots| {
            tairix_parallel::fold_bands(
                runner,
                slots,
                (bands.start, 1),
                true,
                &|band, slot| {
                    slot.iter_mut()
                        .all(|heaps| drawing.fill(deck, band, heaps).is_some())
                },
                &|one, other| one && other,
            )
        });
        if !held {
            return None;
        }
        if bands.end >= WEATHER_CELLS {
            self.gather()?;
        }
        Some(())
    }

    /// Every band's heaps gathered in order, and where each band's start.
    fn gather(&mut self) -> Option<()> {
        let total = self
            .drawn
            .iter()
            .try_fold(0usize, |total, band| total.checked_add(band.len()))?;
        let mut heaps = Vec::new();
        heaps.try_reserve_exact(total).ok()?;
        let mut bands = Vec::new();
        bands.try_reserve_exact(WEATHER_CELLS + 1).ok()?;
        for band in &self.drawn {
            bands.push(u32::try_from(heaps.len()).ok()?);
            heaps.extend_from_slice(band);
        }
        bands.push(u32::try_from(heaps.len()).ok()?);
        self.laid = heaps;
        self.bands = bands;
        self.drawn = Vec::new();
        Some(())
    }

    /// List the gathered heaps over rows `rows` of the level's weather cells
    /// across `runner`; `None` when the heap will not hold the lists.
    pub(super) fn list(&mut self, rows: Range<usize>, runner: &dyn JobRunner) -> Option<()> {
        let mut lists = core::mem::take(&mut self.rows);
        let this = &*self;
        let held = lists.get_mut(rows.clone()).is_some_and(|slots| {
            tairix_parallel::fold_bands(
                runner,
                slots,
                (rows.start, 1),
                true,
                &|row, slot| {
                    slot.iter_mut()
                        .all(|list| this.listed_row(row).map(|listed| *list = listed).is_some())
                },
                &|one, other| one && other,
            )
        });
        self.rows = lists;
        held.then_some(())
    }

    /// Only the heaps `keeps` keeps, listed again; `None` when the heap will
    /// not hold the lists.
    #[cfg(test)]
    pub(super) fn keep(&mut self, keeps: impl Fn(&Heap) -> bool) -> Option<()> {
        let (mut heaps, mut bands) = (Vec::new(), Vec::new());
        for band in self.bands.windows(2) {
            bands.push(u32::try_from(heaps.len()).ok()?);
            let from = usize::try_from(band[0]).ok()?..usize::try_from(band[1]).ok()?;
            heaps.extend(
                self.laid
                    .get(from)?
                    .iter()
                    .copied()
                    .filter(|heap| keeps(heap)),
            );
        }
        bands.push(u32::try_from(heaps.len()).ok()?);
        (self.laid, self.bands) = (heaps, bands);
        self.list(0..WEATHER_CELLS, &tairix_parallel::SERIAL)
    }

    /// Row `row`'s lists: each of its cells' heaps, furthest within first;
    /// `None` when the heap will not hold them.
    fn listed_row(&self, row: usize) -> Option<Row> {
        let (first, near) = self.near(row)?;
        let mut starts = fallible::filled(WEATHER_CELLS + 1, 0u32)?;
        for heap in near {
            self.reach_in(heap, row, |column, _| {
                if let Some(count) = starts.get_mut(column) {
                    *count += 1;
                }
            });
        }
        let mut total = 0u32;
        for count in &mut starts {
            let own = *count;
            *count = total;
            total = total.checked_add(own)?;
        }
        let empty = Member {
            heap: 0,
            most: f32::NEG_INFINITY,
        };
        let mut members = fallible::filled(usize::try_from(total).ok()?, empty)?;
        let mut next = fallible::filled(WEATHER_CELLS + 1, 0u32)?;
        next.copy_from_slice(&starts);
        for (offset, heap) in near.iter().enumerate() {
            let index = u32::try_from(first + offset).ok()?;
            self.reach_in(heap, row, |column, square| {
                if let Some(slot) = next.get_mut(column) {
                    if let Some(member) = usize::try_from(*slot)
                        .ok()
                        .and_then(|at| members.get_mut(at))
                    {
                        *member = Member {
                            heap: index,
                            most: above(self.furthest_within(self.furthest_in(heap, square))),
                        };
                    }
                    *slot += 1;
                }
            });
        }
        for cell in starts.windows(2) {
            let span = usize::try_from(cell[0]).ok()?..usize::try_from(cell[1]).ok()?;
            if let Some(listed) = members.get_mut(span) {
                listed.sort_unstable_by(|one, other| {
                    other
                        .most
                        .total_cmp(&one.most)
                        .then(one.heap.cmp(&other.heap))
                });
            }
        }
        Some(Row { starts, members })
    }

    /// The gathered heaps whose middles lie near enough row `row` of the
    /// level's weather cells for any of their cloud to reach it, a band
    /// either way to spare, and where among them they start.
    fn near(&self, row: usize) -> Option<(usize, &[Heap])> {
        let south = self.corner.1 + real(row) * self.size;
        let first = self
            .drawing
            .band_of(south - self.furthest)
            .saturating_sub(1);
        let last =
            (self.drawing.band_of(south + self.size + self.furthest) + 1).min(WEATHER_CELLS - 1);
        let from = usize::try_from(*self.bands.get(first)?).ok()?;
        let to = usize::try_from(*self.bands.get(last + 1)?).ok()?;
        Some((from, self.laid.get(from..to)?))
    }

    /// The cells of row `row` of the level's weather cells `heap`'s cloud can
    /// stand over, within the margin of its footprint, passed to `visit` by
    /// column with their squares.
    fn reach_in(&self, heap: &Heap, row: usize, mut visit: impl FnMut(usize, Rect)) {
        let reach = (f64::from(heap.radius) + self.margin / self.outside) * self.frame.2;
        let (east, north) = (f64::from(heap.east), f64::from(heap.north));
        let span = |middle: f64, from: f64| {
            let low = (middle - reach - from) / self.size;
            let high = (middle + reach - from) / self.size;
            (high >= 0.0 && low < real(WEATHER_CELLS)).then(|| {
                (
                    cell_of(low).0.min(WEATHER_CELLS - 1),
                    cell_of(high).0.min(WEATHER_CELLS - 1),
                )
            })
        };
        let (Some(columns), Some(rows)) = (span(east, self.corner.0), span(north, self.corner.1))
        else {
            return;
        };
        if !(rows.0..=rows.1).contains(&row) {
            return;
        }
        for column in columns.0..=columns.1 {
            let square = self.square(column, row);
            if apart(heap, square) <= reach {
                visit(column, square);
            }
        }
    }

    /// The square weather cell `(column, row)` covers, its south-west and
    /// north-east corners.
    fn square(&self, column: usize, row: usize) -> Rect {
        let low = (
            self.corner.0 + real(column) * self.size,
            self.corner.1 + real(row) * self.size,
        );
        (low, (low.0 + self.size, low.1 + self.size))
    }

    /// The heaps listed over weather cell `cell`, with the furthest any place
    /// over it lies within each, furthest first.
    fn listed(&self, cell: usize) -> impl Iterator<Item = (&Heap, f64)> {
        let listed = self
            .rows
            .get(cell / WEATHER_CELLS)
            .and_then(|row| {
                let column = cell % WEATHER_CELLS;
                let bound = |at: usize| {
                    row.starts
                        .get(at)
                        .and_then(|&start| usize::try_from(start).ok())
                };
                bound(column)
                    .zip(bound(column + 1))
                    .and_then(|(start, end)| row.members.get(start..end))
            })
            .unwrap_or_default();
        listed.iter().filter_map(|member| {
            usize::try_from(member.heap)
                .ok()
                .and_then(|at| self.laid.get(at))
                .map(|heap| (heap, f64::from(member.most)))
        })
    }

    /// The heaps listed over weather cell `cell`.
    fn at(&self, cell: usize) -> impl Iterator<Item = &Heap> {
        self.listed(cell).map(|(heap, _)| heap)
    }

    /// How a place `altitude` above the scene's level over `(x, z)`, in
    /// weather cell `cell`, stands against the heaps there, where it lies
    /// further within one than `beyond` outside it: as within the heap it
    /// lies furthest within, their envelopes standing together; `None`
    /// where it lies that far outside every one.
    pub(super) fn placed(
        &self,
        cell: usize,
        (x, z): (f64, f64),
        altitude: f64,
        beyond: f64,
    ) -> Option<Placed> {
        let (cos, sin, stretch) = self.frame;
        let mut best: Option<Placed> = None;
        for (heap, most) in self.listed(cell) {
            let worth = best.map_or(-beyond, |held| held.inside);
            // The rest lie no further within, listed as they are.
            if most <= worth {
                break;
            }
            let (east, north) = (x - f64::from(heap.east), z - f64::from(heap.north));
            let (along, across) = (east * cos + north * sin, north * cos - east * sin);
            let edge = f64::from(heap.radius) - mathf::hypot(along / stretch, across);
            if self.furthest_within(edge) <= worth {
                continue;
            }
            if let Some(placed) = Placed::of(edge, altitude - f64::from(heap.base), heap.shape())
                .filter(|placed| placed.inside > worth)
            {
                best = Some(placed);
            }
        }
        best
    }

    /// The heights between which any heap's cloud listed over weather cell
    /// `(column, row)` can stand over it, its turrets and fringe reaching
    /// `margin` past its envelope: rounded out to whole metres, which single
    /// precision holds exactly; empty where none can.
    pub(super) fn band(&self, (column, row): (usize, usize), margin: f64) -> (f32, f32) {
        let square = self.square(column, row);
        let (mut lowest, mut highest) = (f64::INFINITY, f64::NEG_INFINITY);
        for heap in self.at(row * WEATHER_CELLS + column) {
            let shape = heap.shape();
            let Some(rise) = form::highest_rise(self.furthest_in(heap, square), shape, margin)
            else {
                continue;
            };
            let base = f64::from(heap.base);
            lowest = lowest.min(base);
            highest = highest.max((base + rise).min(base + shape.0 + margin));
        }
        if lowest > highest {
            return EMPTY_BAND;
        }
        (single(mathf::floor(lowest)), single(mathf::ceil(highest)))
    }

    /// The most extinction any heap listed over cells `columns` of rows
    /// `rows` holds between heights `low` and `high` over the square
    /// `square`, read through `footprint` metres of `deck`'s cloud whose
    /// turrets push out no further than `reach` there: each heap whose
    /// envelope leaves room for its cloud there no denser than its water risen
    /// from its base to the top of the span or of its cloud.
    pub(super) fn densest(
        &self,
        (columns, rows): Cells,
        square: Rect,
        (low, high): (f64, f64),
        (deck, reach, footprint): (&Deck, f64, f64),
    ) -> f64 {
        let above = reach + form::Edge::widest(deck, footprint).outer();
        let mut most = 0.0f64;
        for row in rows {
            for column in columns.clone() {
                for heap in self.at(row * WEATHER_CELLS + column) {
                    let base = f64::from(heap.base);
                    let shape = heap.shape();
                    let top = base + shape.0 + above;
                    if high <= base || low >= top {
                        continue;
                    }
                    let edge = self.furthest_in(heap, square);
                    if form::may_hold(deck, (edge, shape, low - base), (reach, footprint)) {
                        most = most.max(form::extinction(deck, high.min(top) - base));
                    }
                }
            }
        }
        most
    }

    /// The furthest a place `edge` within a heap's footprint, negative
    /// outside, can lie within its envelope, whatever its height: no further
    /// than in from the footprint's edge, and outside no nearer than the
    /// envelope's fall leaves it.
    fn furthest_within(&self, edge: f64) -> f64 {
        if edge < 0.0 {
            edge * self.outside
        } else {
            edge
        }
    }

    /// The furthest any of the square `square` lies within `heap`'s
    /// footprint: its nearest, as near as the heap's stretch can draw its
    /// footprint toward it.
    fn furthest_in(&self, heap: &Heap, square: Rect) -> f64 {
        f64::from(heap.radius) - apart(heap, square) / self.frame.2
    }
}

/// How far `heap`'s middle lies from the nearest of the square `low..high`.
fn apart(heap: &Heap, (low, high): Rect) -> f64 {
    let (east, north) = (f64::from(heap.east), f64::from(heap.north));
    let off = |middle: f64, low: f64, high: f64| (low - middle).max(middle - high).max(0.0);
    mathf::hypot(off(east, low.0, high.0), off(north, low.1, high.1))
}

/// `deck`'s heading, as its cosine and sine, and how many times longer than
/// broad its heaps are drawn out along it.
fn frame_of(deck: &Deck) -> (f64, f64, f64) {
    (
        mathf::cos(deck.heading),
        mathf::sin(deck.heading),
        deck.stretch.max(1.0),
    )
}

/// How broad the lattice cells `deck`'s heaps are drawn over are.
fn lattice_of(deck: &Deck) -> f64 {
    let broad = LATTICE * deck.scale * frame_of(deck).2;
    let held = (1.0 + GATHERING) * heaps_a_cell(deck, broad);
    if held > CROWDED {
        broad * mathf::sqrt(CROWDED / held)
    } else {
        broad
    }
}

/// The mean number of `deck`'s heaps a lattice cell `lattice` across holds:
/// as many as leave the deck's share of the sky covered where heaps fall
/// together as a Poisson field's do.
fn heaps_a_cell(deck: &Deck, lattice: f64) -> f64 {
    let covered = -mathf::ln(1.0 - deck.cover.clamp(0.0, MOST_COVER));
    let footprint = 0.25 * PI * mean_square_breadth(deck) * frame_of(deck).2;
    covered * lattice * lattice / footprint
}

/// The mean of the square of `deck`'s heaps' breadths.
fn mean_square_breadth(deck: &Deck) -> f64 {
    let (least, most) = (NARROWEST * deck.scale, deck.scale);
    let (counted, squared) = (1.0 - SIZES, 3.0 - SIZES);
    let between = |order: f64| (power(most, order) - power(least, order)) / order;
    between(squared) / between(counted)
}

/// The breadth of one of `deck`'s heaps, `draw` of the way through their
/// distribution.
fn breadth_drawn(deck: &Deck, draw: f64) -> f64 {
    let (least, most) = (NARROWEST * deck.scale, deck.scale);
    let order = 1.0 - SIZES;
    let (from, to) = (power(least, order), power(most, order));
    power(from + (to - from) * draw, 1.0 / order)
}

/// How deep one of `deck`'s heaps `breadth` across stands: the deck's least
/// depth for its narrowest heap to its greatest for its broadest, strayed
/// from by `draw`.
fn depth_of(deck: &Deck, breadth: f64, draw: f64) -> f64 {
    let least = NARROWEST * deck.scale;
    let share = ((breadth - least) / (deck.scale - least).max(f64::MIN_POSITIVE)).clamp(0.0, 1.0);
    let depth = deck.depth.0 + (deck.depth.1 - deck.depth.0) * share;
    (depth * (1.0 + DEPTH_STRAY * (2.0 * draw - 1.0))).clamp(deck.depth.0, deck.depth.1)
}

#[cfg(test)]
#[path = "heaps_tests.rs"]
mod tests;
