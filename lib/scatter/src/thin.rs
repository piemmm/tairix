//! A region thinned row by row, round by round.

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::cmp::Ordering;

use tairix_parallel::JobRunner;

use crate::{Cell, Error, Offers, Span};

/// Cells a step reads, and works on, for each of the runner's cores: few
/// enough that offers each read from a land in a few microseconds keep a
/// step within a few milliseconds.
const BATCH: usize = 1 << 10;

/// Cells a core reads or works on at once: rows are shared out in pieces this
/// long, however few rows a wave takes, so a wide row keeps every core busy.
const PIECE: usize = 1 << 9;

/// Columns of a row read that share one highest rank, so a search for an
/// outranking neighbour passes over those none of which could.
const STRIP: usize = 8;

/// The most rounds a thinning runs: each round's phases are counted in a
/// cell's byte.
pub const MOST_ROUNDS: u32 = 64;

/// The furthest from the lattice's origin, on either axis, a span may lie:
/// so far within a word's reach that every row, column and width a thinning
/// reckons from it, a few of its widest halo out, is counted exactly.
pub const FARTHEST: i64 = 1 << 60;

/// A cell's offer and its rank, as read.
type Held<Offer, Rank> = Option<(Offer, Rank)>;

/// What became of a cell's offer: undecided, or the phase that decided it —
/// kept in round `r` at phase `2r − 1`, yielded in round `r` at phase `2r`.
type Fate = u8;

const UNDECIDED: Fate = 0;

/// The highest rank of a strip's offers still undecided, and their bound
/// joined; `None` where none is.
type Strip<Rank, Bound> = Option<(Rank, Bound)>;

/// A piece of a row to read: its row, its first column, its cells, and their
/// strips.
type Piece<'a, Offer, Rank, Bound> = (
    i64,
    i64,
    &'a mut [Held<Offer, Rank>],
    &'a mut [Strip<Rank, Bound>],
);

/// A row of offers read: its row, its first column, each cell's offer and
/// rank, what became of each cell, and how many phases the row has been
/// through; for each strip of it, what its offers still undecided when the
/// row's round began rank and are bound by, and the bound of those its last
/// keeping kept, if any; and the whole row's undecided offers so measured.
struct Read<Offer, Rank, Bound> {
    row: i64,
    first: i64,
    cells: Vec<Held<Offer, Rank>>,
    fates: Vec<Fate>,
    phase: u8,
    strips: Vec<Strip<Rank, Bound>>,
    kept: Vec<Option<Bound>>,
    whole: Strip<Rank, Bound>,
}

/// `strip` with the offer ranked `rank` and bound by `bound` joined to it.
fn joined<O: Offers>(
    offers: &O,
    strip: Strip<O::Rank, O::Bound>,
    (rank, bound): (O::Rank, O::Bound),
) -> (O::Rank, O::Bound) {
    strip.map_or((rank, bound), |(highest, held)| {
        (highest.max(rank), offers.join(held, bound))
    })
}

impl<Offer, Rank: Ord + Copy, Bound: Copy> Read<Offer, Rank, Bound> {
    /// The offer and rank held at `column`, if any.
    fn held(&self, column: i64) -> Option<&(Offer, Rank)> {
        self.cells.get(self.index(column)?)?.as_ref()
    }

    /// What became of the offer at `column`.
    fn fate(&self, column: i64) -> Fate {
        self.index(column)
            .and_then(|index| self.fates.get(index))
            .copied()
            .unwrap_or(UNDECIDED)
    }

    fn index(&self, column: i64) -> Option<usize> {
        usize::try_from(column.checked_sub(self.first)?).ok()
    }

    /// The column of the cell at `index`.
    fn column(&self, index: usize) -> i64 {
        self.first + i64::try_from(index).unwrap_or(i64::MAX)
    }

    /// The indices of its columns from `from` to `to`.
    fn among(&self, (from, to): (i64, i64)) -> core::ops::Range<usize> {
        let start = usize::try_from(from.saturating_sub(self.first)).unwrap_or(0);
        let end = usize::try_from(to.saturating_sub(self.first))
            .unwrap_or(0)
            .min(self.cells.len());
        start..end.max(start)
    }

    /// Whether the cells from index `index` to `past`, whose offers `bound`
    /// holds, lie near enough `offer`, at `cell`, for one to crowd it.
    fn near<O: Offers<Offer = Offer, Rank = Rank, Bound = Bound>>(
        &self,
        offers: &O,
        (offer, cell): (&Offer, Cell),
        (bound, index, past): (Bound, usize, usize),
    ) -> bool {
        let rows = (self.row - cell.1).abs();
        let columns = (self.column(index) - cell.0)
            .max(cell.0 - self.column(past.saturating_sub(1)))
            .max(0);
        let cells = u32::try_from(rows.max(columns)).unwrap_or(u32::MAX);
        offers.within(offer, bound, cells)
    }

    /// Whether an offer among its columns from `from` to `to`, still
    /// undecided before round `round`, outranks the one held at `cell`,
    /// ranked `rank`, and crowds it: ranked higher, or alike and in a later
    /// row or, in the same row, a later column.
    fn outranks<O: Offers<Offer = Offer, Rank = Rank, Bound = Bound>>(
        &self,
        offers: &O,
        (offer, rank, cell): (&Offer, Rank, Cell),
        columns: (i64, i64),
        round: u8,
    ) -> bool {
        // A strip whose highest rank only matches may hold a later cell
        // ranked alike, which outranks.
        let reaches = |strip: Strip<Rank, Bound>, (index, past): (usize, usize)| {
            strip.is_some_and(|(highest, bound)| {
                highest >= rank && self.near(offers, (offer, cell), (bound, index, past))
            })
        };
        let among = self.among(columns);
        if among.is_empty() || !reaches(self.whole, (among.start, among.end)) {
            return false;
        }
        // Undecided before this round, or decided in it or since.
        let open = |fate: Fate| fate == UNDECIDED || fate >= 2 * round - 1;
        let mut index = among.start;
        while index < among.end {
            let strip = index / STRIP;
            let past = ((strip + 1) * STRIP).min(among.end);
            if reaches(self.strips.get(strip).copied().flatten(), (index, past)) {
                let cells = self.cells.get(index..past).unwrap_or_default();
                let fates = self.fates.get(index..past).unwrap_or_default();
                for (at, (held, &fate)) in (index..).zip(cells.iter().zip(fates)) {
                    let Some((other, other_rank)) = held else {
                        continue;
                    };
                    let outranking = match other_rank.cmp(&rank) {
                        Ordering::Greater => true,
                        Ordering::Less => false,
                        Ordering::Equal => (self.row, self.column(at)) > (cell.1, cell.0),
                    };
                    if outranking && open(fate) && offers.crowds(offer, other) {
                        return true;
                    }
                }
            }
            index = past;
        }
        false
    }

    /// Whether an offer among its columns from `from` to `to` kept in round
    /// `round` crowds `offer`.
    fn keeps<O: Offers<Offer = Offer, Rank = Rank, Bound = Bound>>(
        &self,
        offers: &O,
        (offer, cell): (&Offer, Cell),
        columns: (i64, i64),
        round: u8,
    ) -> bool {
        let kept = 2 * round - 1;
        let among = self.among(columns);
        let mut index = among.start;
        while index < among.end {
            let strip = index / STRIP;
            let past = ((strip + 1) * STRIP).min(among.end);
            let reaches = self
                .kept
                .get(strip)
                .copied()
                .flatten()
                .is_some_and(|bound| self.near(offers, (offer, cell), (bound, index, past)));
            if reaches {
                let crowded = (index..past).any(|at| {
                    self.fates.get(at) == Some(&kept)
                        && self
                            .cells
                            .get(at)
                            .and_then(Option::as_ref)
                            .is_some_and(|(other, _)| offers.crowds(offer, other))
                });
                if crowded {
                    return true;
                }
            }
            index = past;
        }
        false
    }

    /// Take its strips' measure afresh as phase `phase` leaves them: after a
    /// round's yielding, what the offers still undecided in each rank and are
    /// bound by; after its keeping, the bound of those each kept.
    fn measure<O: Offers<Offer = Offer, Rank = Rank, Bound = Bound>>(
        &mut self,
        offers: &O,
        phase: u8,
    ) {
        if phase % 2 == 1 {
            let strips = self
                .cells
                .chunks(STRIP)
                .zip(self.fates.chunks(STRIP))
                .zip(self.kept.iter_mut());
            for ((cells, fates), kept) in strips {
                *kept = cells
                    .iter()
                    .zip(fates)
                    .filter(|&(_, &fate)| fate == phase)
                    .filter_map(|(held, _)| held.as_ref())
                    .map(|(offer, _)| offers.bound(offer))
                    .reduce(|one, other| offers.join(one, other));
            }
            return;
        }
        let strips = self
            .cells
            .chunks(STRIP)
            .zip(self.fates.chunks(STRIP))
            .zip(self.strips.iter_mut());
        for ((cells, fates), strip) in strips {
            *strip = cells
                .iter()
                .zip(fates)
                .filter(|&(_, &fate)| fate == UNDECIDED)
                .filter_map(|(held, _)| held.as_ref())
                .fold(None, |strip, (offer, rank)| {
                    Some(joined(offers, strip, (*rank, offers.bound(offer))))
                });
        }
        self.whole = self
            .strips
            .iter()
            .flatten()
            .fold(None, |whole, &strip| Some(joined(offers, whole, strip)));
    }
}

/// How far along a row the `piece`th piece begins.
fn offset(piece: usize) -> i64 {
    i64::try_from(piece.saturating_mul(PIECE)).unwrap_or(i64::MAX)
}

/// A region's cells kept or dropped a batch of rows at a time, round by
/// round: in each round an offer no undecided outranking offer within its
/// reach crowds is kept, and every undecided offer a kept one crowds yields;
/// what is still undecided after the last round yields too. One round is
/// Matérn's second process; enough rounds keep exactly what thinning the
/// offers one by one from the highest ranked would.
///
/// It holds only what it has read, never the offers that read it, so the
/// offers may borrow what changes between one step and the next — so long as
/// every step is handed offers that answer alike.
pub struct Thinning<Offer, Rank, Bound> {
    spans: Vec<Span>,
    reach: i64,
    rounds: u8,
    /// How far, in rows and columns, a cell's fate reaches back to the
    /// offers it hangs on: `2 × rounds − 1` reaches.
    halo: i64,
    /// The rows read that a span still to be ruled on hangs on, in order.
    read: VecDeque<Read<Offer, Rank, Bound>>,
    /// The next row that may need reading, and the next span to rule on.
    row: i64,
    next: usize,
}

impl<Offer, Rank, Bound> core::fmt::Debug for Thinning<Offer, Rank, Bound> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("Thinning")
            .field("spans", &self.spans.len())
            .field("reach", &self.reach)
            .field("rounds", &self.rounds)
            .field("next", &self.next)
            .finish_non_exhaustive()
    }
}

/// A row to read: its row, its first column, and how many columns.
type Planned = (i64, i64, usize);

/// A piece of a wave: the row read it decides, its first column, and a
/// verdict for each of its cells.
type Verdicts<'a> = (usize, i64, &'a mut [bool]);

impl<Offer, Rank, Bound> Thinning<Offer, Rank, Bound>
where
    Offer: Clone + Send + Sync,
    Rank: Ord + Copy + Send + Sync,
    Bound: Copy + Send + Sync,
{
    /// The thinning of the cells `spans` hold, rows increasing, under offers
    /// reaching `reach` cells, over `rounds` rounds; `Shape` for rows out of
    /// order, a span lying further from the lattice's origin than
    /// [`FARTHEST`], or no rounds or more than [`MOST_ROUNDS`].
    pub fn new(spans: Vec<Span>, reach: u32, rounds: u32) -> Result<Self, Error> {
        if rounds == 0 || rounds > MOST_ROUNDS {
            return Err(Error::Shape);
        }
        let reach = i64::from(reach);
        let halo = (2 * i64::from(rounds) - 1) * reach;
        let near = |at: i64| (-FARTHEST..=FARTHEST).contains(&at);
        let fits = |span: &Span| {
            span.columns.0 <= span.columns.1
                && near(span.columns.0)
                && near(span.columns.1)
                && near(span.row)
        };
        let ordered = spans.windows(2).all(|pair| pair[0].row < pair[1].row);
        if !ordered || !spans.iter().all(fits) {
            return Err(Error::Shape);
        }
        let row = spans.first().map_or(0, |span| span.row - halo);
        Ok(Self {
            spans,
            reach,
            rounds: u8::try_from(rounds).map_err(|_| Error::Shape)?,
            halo,
            read: VecDeque::new(),
            row,
            next: 0,
        })
    }

    /// The share of its rows ruled on, `0.0..=1.0`.
    #[must_use]
    pub fn done(&self) -> f64 {
        tairix_util::mathf::share(self.next, self.spans.len())
    }

    /// Read the next rows and carry the rows read on through their rounds,
    /// about a batch of cells each for each of `runner`'s cores, adding each
    /// cell kept, with its offer, to `kept` in its rows' and columns' order
    /// once its last round is done: whether every row is ruled on. A refusal
    /// leaves it as its last whole read or wave left it, and called again it
    /// goes on from there; `Shape` for offers reaching further than it was
    /// drawn for.
    pub fn step<O>(
        &mut self,
        offers: &O,
        runner: &dyn JobRunner,
        kept: &mut Vec<(Cell, Offer)>,
    ) -> Result<bool, Error>
    where
        O: Offers<Offer = Offer, Rank = Rank, Bound = Bound>,
    {
        // Drawn for a nearer reach than its offers', it would keep two that
        // crowd each other.
        if i64::from(offers.reach()) > self.reach {
            return Err(Error::Shape);
        }
        let budget = BATCH.saturating_mul(runner.width().max(1));
        let mut work = 0usize;
        while self.next < self.spans.len() && work < budget {
            // Rows are read only once none read can go on, so no more are
            // held than a span's fate hangs on and a batch beyond.
            let mut done = self.wave(offers, runner, budget - work)?;
            if done == 0 {
                done = self.read_rows(offers, runner, budget - work)?;
            }
            let next = self.next;
            self.rule(kept)?;
            if done == 0 && self.next == next {
                // Nothing left to read and no row able to go on: spans no
                // rows of which can ever be decided.
                return Err(Error::Shape);
            }
            work = work.saturating_add(done);
            let keep_from = self
                .spans
                .get(self.next)
                .map_or(i64::MAX, |span| span.row - self.halo);
            while self.read.front().is_some_and(|read| read.row < keep_from) {
                self.read.pop_front();
            }
        }
        let finished = self.next >= self.spans.len();
        if finished {
            self.read = VecDeque::new();
        }
        Ok(finished)
    }

    /// The last phase a row goes through: the last round's keeping.
    fn last(&self) -> u8 {
        2 * self.rounds - 1
    }

    /// The first row from `from` on that a span within its halo of it needs.
    fn needed_from(&self, from: i64) -> Option<i64> {
        let index = self
            .spans
            .partition_point(|span| span.row + self.halo < from);
        let span = self.spans.get(index)?;
        Some(from.max(span.row - self.halo))
    }

    /// The columns row `row` is read across: from the first to past the last
    /// that a span within its halo of it holds, with the halo either side.
    fn needed_columns(&self, row: i64) -> Option<(i64, i64)> {
        let from = self
            .spans
            .partition_point(|span| span.row < row - self.halo);
        self.spans
            .get(from..)?
            .iter()
            .take_while(|span| span.row <= row + self.halo)
            .map(|span| (span.columns.0 - self.halo, span.columns.1 + self.halo))
            .reduce(|(a, b), (c, d)| (a.min(c), b.max(d)))
    }

    /// Read the rows the next spans need, about `budget` cells of them, in
    /// pieces across `runner`: how many cells were read. Nothing is kept of a
    /// read the heap refused.
    fn read_rows<O: Offers<Offer = Offer, Rank = Rank, Bound = Bound>>(
        &mut self,
        offers: &O,
        runner: &dyn JobRunner,
        budget: usize,
    ) -> Result<usize, Error> {
        let mut planned: Vec<Planned> = Vec::new();
        let (mut row, mut cells) = (self.row, 0usize);
        while cells < budget {
            let Some(next) = self.needed_from(row) else {
                break;
            };
            let (first, past) = self.needed_columns(next).ok_or(Error::Shape)?;
            let count = usize::try_from(past - first).map_err(|_| Error::Shape)?;
            planned.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
            planned.push((next, first, count));
            cells = cells.saturating_add(count.max(1));
            row = next + 1;
        }
        let mut rows: Vec<Read<Offer, Rank, Bound>> = Vec::new();
        rows.try_reserve_exact(planned.len())
            .map_err(|_| Error::OutOfMemory)?;
        let mut pieces = 0usize;
        for &(row, first, count) in &planned {
            rows.push(Read {
                row,
                first,
                cells: filled(count, None)?,
                fates: filled(count, UNDECIDED)?,
                phase: 0,
                strips: filled(count.div_ceil(STRIP), None)?,
                kept: filled(count.div_ceil(STRIP), None)?,
                whole: None,
            });
            pieces += count.div_ceil(PIECE);
        }
        let mut work: Vec<Piece<'_, Offer, Rank, Bound>> = Vec::new();
        work.try_reserve_exact(pieces)
            .map_err(|_| Error::OutOfMemory)?;
        for read in &mut rows {
            let (row, first) = (read.row, read.first);
            let parts = read
                .cells
                .chunks_mut(PIECE)
                .zip(read.strips.chunks_mut(PIECE / STRIP))
                .enumerate();
            for (piece, (cells, strips)) in parts {
                work.push((row, first + offset(piece), cells, strips));
            }
        }
        tairix_parallel::for_each(runner, &mut work, &|(row, column, cells, strips)| {
            for (at, slot) in (*column..).zip(cells.iter_mut()) {
                *slot = offers.offer((at, *row)).map(|offer| {
                    let rank = offers.rank(&offer);
                    (offer, rank)
                });
            }
            for (cells, strip) in cells.chunks(STRIP).zip(strips.iter_mut()) {
                *strip = cells.iter().flatten().fold(None, |strip, (offer, rank)| {
                    Some(joined(offers, strip, (*rank, offers.bound(offer))))
                });
            }
        });
        drop(work);
        for read in &mut rows {
            read.whole = read
                .strips
                .iter()
                .flatten()
                .fold(None, |whole, &strip| Some(joined(offers, whole, strip)));
        }
        self.read
            .try_reserve(rows.len())
            .map_err(|_| Error::OutOfMemory)?;
        if let Some(last) = rows.last() {
            self.row = last.row + 1;
        }
        self.read.extend(rows);
        Ok(cells)
    }

    /// Whether row `index` of those read may go through its next phase:
    /// every row it will ever read within reach is read, and none of those
    /// is behind it. A row never read stands for no offers at all: the
    /// rows a span's fate hangs on are always read.
    fn ready(&self, index: usize) -> bool {
        let Some(read) = self.read.get(index) else {
            return false;
        };
        if read.phase >= self.last()
            || self
                .needed_from(self.row)
                .is_some_and(|unread| unread <= read.row + self.reach)
        {
            return false;
        }
        let near = |other: &&Read<Offer, Rank, Bound>| (other.row - read.row).abs() <= self.reach;
        let below = self.read.range(..index).rev().take_while(near);
        let above = self.read.range(index + 1..).take_while(near);
        below.chain(above).all(|other| other.phase >= read.phase)
    }

    /// Carry the rows read that may go on through their next phase, the
    /// lowest first and about `budget` cells of them, across `runner`: how
    /// many cells were worked on, nought where no row may go on. A wave
    /// decides each of its rows from the fates the rows about it held before
    /// it, which a round's decisions never change the answer of, so its rows
    /// are decided alike whichever core takes them and in whatever order.
    fn wave<O: Offers<Offer = Offer, Rank = Rank, Bound = Bound>>(
        &mut self,
        offers: &O,
        runner: &dyn JobRunner,
        budget: usize,
    ) -> Result<usize, Error> {
        let mut wave: Vec<(usize, usize)> = Vec::new();
        let mut cells = 0usize;
        for index in 0..self.read.len() {
            if cells >= budget {
                break;
            }
            if self.ready(index) {
                let count = self.read.get(index).map_or(0, |read| read.cells.len());
                wave.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
                wave.push((index, cells));
                cells += count;
            }
        }
        if wave.is_empty() {
            return Ok(0);
        }
        let mut verdicts: Vec<bool> = filled(cells, false)?;
        let mut pieces: Vec<Verdicts<'_>> = Vec::new();
        pieces
            .try_reserve_exact(cells.div_ceil(PIECE) + wave.len())
            .map_err(|_| Error::OutOfMemory)?;
        let mut rest = verdicts.as_mut_slice();
        for &(index, _) in &wave {
            let read = self.read.get(index).ok_or(Error::Shape)?;
            let (mine, tail) = rest.split_at_mut(read.cells.len());
            rest = tail;
            for (piece, verdicts) in mine.chunks_mut(PIECE).enumerate() {
                pieces.push((index, read.first + offset(piece), verdicts));
            }
        }
        let (reach, read) = (self.reach, &self.read);
        tairix_parallel::for_each(runner, &mut pieces, &|(index, column, verdicts)| {
            let Some(row) = read.get(*index) else {
                return;
            };
            let round = row.phase / 2 + 1;
            let keeping = row.phase % 2 == 0;
            let low = read.partition_point(|other| other.row < row.row - reach);
            let around = read
                .range(low..)
                .take_while(|other| other.row <= row.row + reach);
            for (column, verdict) in (*column..).zip(verdicts.iter_mut()) {
                let Some((offer, rank)) = row.held(column) else {
                    continue;
                };
                if row.fate(column) != UNDECIDED {
                    continue;
                }
                let (near, cell) = ((column - reach, column + reach + 1), (column, row.row));
                *verdict = if keeping {
                    !around
                        .clone()
                        .any(|other| other.outranks(offers, (offer, *rank, cell), near, round))
                } else {
                    around
                        .clone()
                        .any(|other| other.keeps(offers, (offer, cell), near, round))
                };
            }
        });
        drop(pieces);
        for &(index, start) in &wave {
            let read = self.read.get_mut(index).ok_or(Error::Shape)?;
            let decided = read.phase + 1;
            let verdicts = verdicts
                .get(start..start + read.cells.len())
                .ok_or(Error::Shape)?;
            for (fate, &verdict) in read.fates.iter_mut().zip(verdicts) {
                if verdict {
                    *fate = decided;
                }
            }
            read.phase = decided;
            read.measure(offers, decided);
        }
        Ok(cells.max(1))
    }

    /// Rule on the spans whose rows have been through every round, adding
    /// what they keep to `kept`; nothing is ruled on when the heap refuses
    /// room for what is kept.
    fn rule(&mut self, kept: &mut Vec<(Cell, Offer)>) -> Result<(), Error> {
        let last = self.last();
        let mut end = self.next;
        let mut standing = 0usize;
        while let Some(span) = self.spans.get(end) {
            let Some(read) = self.row_of(span.row) else {
                break;
            };
            if read.phase < last {
                break;
            }
            standing += (span.columns.0..span.columns.1)
                .filter(|&column| read.fate(column) % 2 == 1)
                .count();
            end += 1;
        }
        kept.try_reserve(standing).map_err(|_| Error::OutOfMemory)?;
        for span in self.spans.get(self.next..end).ok_or(Error::Shape)? {
            let read = self.row_of(span.row).ok_or(Error::Shape)?;
            for column in span.columns.0..span.columns.1 {
                if read.fate(column) % 2 == 1 {
                    if let Some((offer, _)) = read.held(column) {
                        kept.push(((column, span.row), offer.clone()));
                    }
                }
            }
        }
        self.next = end;
        Ok(())
    }

    /// The row read for row `row`, if it is held.
    fn row_of(&self, row: i64) -> Option<&Read<Offer, Rank, Bound>> {
        let index = self.read.partition_point(|read| read.row < row);
        self.read.get(index).filter(|read| read.row == row)
    }
}

/// `count` copies of `value`, taken fallibly.
fn filled<T: Clone>(count: usize, value: T) -> Result<Vec<T>, Error> {
    let mut items = Vec::new();
    items
        .try_reserve_exact(count)
        .map_err(|_| Error::OutOfMemory)?;
    items.resize(count, value);
    Ok(items)
}

#[cfg(test)]
#[path = "thin_tests.rs"]
mod tests;
