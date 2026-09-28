//! Conway's Game of Life on a torus, drawn as cells that are born white-hot,
//! settle into their colony's colour as they age, and die as fading embers.
//!
//! Every cell carries one of four colours, and a newborn takes the colour
//! most of its three parents share — or, when all three differ, the one none
//! of them has, as four-colour Life has it. Colour never decides who lives, so
//! the dynamics are Conway's B3/S23 exactly and the colonies' borders are
//! where their histories meet.
//!
//! The board is bit-packed, 64 cells a word, and a generation is computed a
//! word at a time by bit-sliced addition of the eight neighbours. A world
//! that has settled — into a cycle, into near-stillness, or into
//! near-emptiness — is reseeded after a grace, so the screen never rests on a
//! dead or blinking pattern.
//!
//! A frame repaints only the cells whose look is still changing: one being
//! born or dying, or one ageing into its next shade — however coarsely the
//! damage the compositor is told about is kept. Under reduced motion a
//! cell is born and dies at once rather than fading.

use alloc::vec::Vec;

use tairix_rng::{NonCryptoRng, RandU64};
use tairix_util::fallible;
use tairix_wallpaper::LifeOptions;
use tairix_wm::{Color, Compositor, Rect, Region, Scale, Surface, WindowId};

use super::{seed_from, SAVER_FRAME_NS};

/// The most cells a board holds: past it the cells grow instead, so a
/// generation and a frame cost the same on a very large screen.
const MAX_CELLS: u64 = 1 << 18;

/// How much of its full brightness a cell gains a frame while being born, and
/// loses while dying, at three frames a generation: born in one generation,
/// gone in about three. A faster or slower world scales both by its own pace.
const BIRTH_STEP: u8 = 86;
const DEATH_STEP: u8 = 30;
const PACE_OF_STEPS: u32 = 3;

/// Out of 1 000, how much of a fresh board is alive.
const SEED_DENSITY: u64 = 330;

/// The colours a colony can have.
const FAMILIES: usize = 4;

/// Each colony's colour at its full depth: aurora teal, violet, amber, rose.
const HUES: [(u8, u8, u8); FAMILIES] = [
    (0x2C, 0xDC, 0xC4),
    (0x96, 0x6E, 0xFF),
    (0xFF, 0xB0, 0x3C),
    (0xFF, 0x5A, 0x8C),
];

/// The ages a shade begins at: newborn, young, adult, old, ancient.
const SHADE_AGES: [u8; 5] = [0, 2, 4, 16, 64];

/// Each shade's mix, out of 255: positive towards white, negative towards
/// black.
const SHADE_MIX: [i16; 5] = [170, 90, 0, -60, -115];

/// Generations of history a repeat is looked for in: a cycle of up to this
/// period is recognised as one.
const HISTORY: usize = 64;

/// Generations a world may change almost nothing before it counts as
/// settled.
const QUIET_LIMIT: u32 = 120;

/// Generations a settled world is left before it is reseeded.
const SETTLED_GRACE: u32 = 40;

/// The damage budget, past which a frame's damage is its bounding box.
const DAMAGE_BUDGET: usize = 64;

/// The Game of Life screensaver.
pub(super) struct Life {
    cols: usize,
    rows: usize,
    /// Words per row.
    words: usize,
    /// The valid bits of each row's last word.
    tail_mask: u64,
    cell: u32,
    gap: u32,
    radius: u32,
    /// The board's top-left on the screen: the margin left over when the
    /// screen is not a whole number of cells, split either side.
    origin: (u32, u32),
    board: Vec<u64>,
    next: Vec<u64>,
    family: Vec<u8>,
    age: Vec<u8>,
    /// How brightly each cell is drawn now, `0..=255`.
    level: Vec<u8>,
    /// The cells whose look is still changing, which the next frame draws.
    active: Vec<u32>,
    /// Which cells [`active`](Self::active) holds, one bit a cell.
    queued: Vec<u64>,
    palette: [[Color; SHADE_AGES.len()]; FAMILIES],
    /// Digests of the last [`HISTORY`] generations.
    history: Vec<u64>,
    recorded: usize,
    quiet: u32,
    settled: u32,
    rng: NonCryptoRng,
    damage: Region,
    frames: u32,
    /// Frames between generations.
    pace: u32,
    /// How much of its brightness a cell gains a frame while being born, and
    /// loses while dying.
    steps: (u8, u8),
    size: (u32, u32),
    due_ns: u64,
}

impl Life {
    /// A freshly seeded world for a `size` screen at `scale` as `options`
    /// describe it, first drawn at `now_ns` and `calm` under reduced motion;
    /// `None` when the screen holds no cell or the heap will not give the
    /// board.
    pub(super) fn new(
        size: (u32, u32),
        scale: Scale,
        (calm, options): (bool, LifeOptions),
        now_ns: u64,
    ) -> Option<Self> {
        let (width, height) = size;
        let pixels = u64::from(width) * u64::from(height);
        let bound = u32::try_from(pixels.div_ceil(MAX_CELLS).isqrt() + 1).unwrap_or(u32::MAX);
        let cell = scale
            .scale_length(options.cells.logical_side())
            .max(bound)
            .max(3);
        let generation_ns = 1_000_000_000 / u64::from(options.speed.per_second().max(1));
        let pace = u32::try_from(generation_ns / SAVER_FRAME_NS)
            .unwrap_or(u32::MAX)
            .max(1);
        let scaled = |step: u8| {
            u8::try_from((u32::from(step) * PACE_OF_STEPS).div_ceil(pace)).unwrap_or(u8::MAX)
        };
        let cols = usize::try_from(width / cell).ok()?;
        let rows = usize::try_from(height / cell).ok()?;
        if cols < 3 || rows < 3 {
            return None;
        }
        let words = cols.div_ceil(64);
        let cells = cols.checked_mul(rows)?;
        let tail_bits = cols - (words - 1) * 64;
        let mut life = Self {
            cols,
            rows,
            words,
            tail_mask: if tail_bits == 64 {
                u64::MAX
            } else {
                (1u64 << tail_bits) - 1
            },
            cell,
            gap: (cell / 8).max(1),
            radius: cell / 4,
            origin: (
                (width - u32::try_from(cols).ok()? * cell) / 2,
                (height - u32::try_from(rows).ok()? * cell) / 2,
            ),
            board: fallible::filled(rows.checked_mul(words)?, 0)?,
            next: fallible::filled(rows * words, 0)?,
            family: fallible::filled(cells, 0)?,
            age: fallible::filled(cells, 0)?,
            level: fallible::filled(cells, 0)?,
            active: Vec::new(),
            queued: fallible::filled(cells.div_ceil(64), 0)?,
            palette: palette(),
            history: fallible::filled(HISTORY, 0)?,
            recorded: 0,
            quiet: 0,
            settled: 0,
            rng: NonCryptoRng::seed_from_u64(seed_from(now_ns)),
            damage: Region::with_budget(DAMAGE_BUDGET),
            frames: 0,
            pace,
            steps: if calm {
                (u8::MAX, u8::MAX)
            } else {
                (scaled(BIRTH_STEP), scaled(DEATH_STEP))
            },
            size,
            due_ns: now_ns,
        };
        if !fallible::reserve(&mut life.active, cells) {
            return None;
        }
        life.reseed();
        Some(life)
    }

    /// When the next frame is due.
    pub(super) const fn due_ns(&self) -> u64 {
        self.due_ns
    }

    /// Step the world and its fades to `now_ns` and draw the frame, if one
    /// is due.
    pub(super) fn advance(&mut self, now_ns: u64, wm: WindowId, compositor: &mut Compositor) {
        if now_ns < self.due_ns {
            return;
        }
        self.due_ns = now_ns.saturating_add(SAVER_FRAME_NS);
        self.frames += 1;
        if self.frames >= self.pace {
            self.frames = 0;
            self.generation();
        }
        self.fade();
        let size = self.size;
        let damage = core::mem::take(&mut self.damage);
        // A kept buffer differs from this frame in the changing cells alone.
        let kept = compositor.keeps_content(wm, size);
        let _ = compositor.repaint_window(wm, size, &damage, |surface, rects| {
            if !kept {
                for rect in rects {
                    self.paint(surface, *rect);
                }
                return;
            }
            for &cell in &self.active {
                let cell = cell as usize;
                self.repaint_cell(surface, cell / self.cols, cell % self.cols);
            }
        });
        self.damage = damage;
        self.retire_settled();
    }

    /// Compute the next generation and mark every cell whose look it
    /// changes.
    fn generation(&mut self) {
        let mut changed = 0usize;
        for row in 0..self.rows {
            let up = (row + self.rows - 1) % self.rows;
            let down = (row + 1) % self.rows;
            for word in 0..self.words {
                let north = self.board[up * self.words + word];
                let centre = self.board[row * self.words + word];
                let south = self.board[down * self.words + word];
                let sum = neighbour_count([
                    north,
                    south,
                    self.west(up, word),
                    self.east(up, word),
                    self.west(row, word),
                    self.east(row, word),
                    self.west(down, word),
                    self.east(down, word),
                ]);
                let mut next = sum.1 & !sum.2 & (sum.0 | centre);
                if word + 1 == self.words {
                    next &= self.tail_mask;
                }
                self.next[row * self.words + word] = next;
            }
        }
        for row in 0..self.rows {
            for word in 0..self.words {
                let at = row * self.words + word;
                let (was, now) = (self.board[at], self.next[at]);
                changed += (was ^ now).count_ones() as usize;
                self.mark_births(row, word, now & !was);
                self.mark_deaths(row, word, was & !now);
                self.age_survivors(row, word, was & now);
            }
        }
        core::mem::swap(&mut self.board, &mut self.next);
        self.watch_for_settling(changed);
    }

    /// Give each cell born in `bits` its parents' colour and a first shade.
    fn mark_births(&mut self, row: usize, word: usize, mut bits: u64) {
        while bits != 0 {
            let col = word * 64 + bits.trailing_zeros() as usize;
            bits &= bits - 1;
            let cell = row * self.cols + col;
            self.family[cell] = self.inherited_family(row, col);
            self.age[cell] = 0;
            self.queue(cell);
        }
    }

    /// Start each cell dying in `bits` fading out.
    fn mark_deaths(&mut self, row: usize, word: usize, mut bits: u64) {
        while bits != 0 {
            let col = word * 64 + bits.trailing_zeros() as usize;
            bits &= bits - 1;
            self.queue(row * self.cols + col);
        }
    }

    /// Age each cell surviving in `bits`, redrawing one that has aged into
    /// its next shade.
    fn age_survivors(&mut self, row: usize, word: usize, mut bits: u64) {
        while bits != 0 {
            let col = word * 64 + bits.trailing_zeros() as usize;
            bits &= bits - 1;
            let cell = row * self.cols + col;
            let age = self.age[cell].saturating_add(1);
            self.age[cell] = age;
            if SHADE_AGES.contains(&age) {
                self.queue(cell);
            }
        }
    }

    /// The colour a cell born at `(row, col)` takes from its live neighbours
    /// on the current board: the one at least two share, or the one none has
    /// when all three differ.
    fn inherited_family(&self, row: usize, col: usize) -> u8 {
        let mut counts = [0u8; FAMILIES];
        for dr in [self.rows - 1, 0, 1] {
            for dc in [self.cols - 1, 0, 1] {
                if dr == 0 && dc == 0 {
                    continue;
                }
                let (r, c) = ((row + dr) % self.rows, (col + dc) % self.cols);
                if self.alive(r, c) {
                    let family = usize::from(self.family[r * self.cols + c]) % FAMILIES;
                    counts[family] += 1;
                }
            }
        }
        if let Some(shared) = counts.iter().position(|count| *count >= 2) {
            return u8::try_from(shared).unwrap_or(0);
        }
        counts
            .iter()
            .position(|count| *count == 0)
            .and_then(|missing| u8::try_from(missing).ok())
            .unwrap_or(0)
    }

    /// Whether the cell at `(row, col)` is alive on the current board.
    fn alive(&self, row: usize, col: usize) -> bool {
        self.board[row * self.words + col / 64] >> (col % 64) & 1 == 1
    }

    /// Row `row`'s word `word`, each bit replaced by its western neighbour,
    /// the board wrapping round.
    fn west(&self, row: usize, word: usize) -> u64 {
        let base = row * self.words;
        let carry = if word == 0 {
            let last = self.cols - 1;
            self.board[base + last / 64] >> (last % 64) & 1
        } else {
            self.board[base + word - 1] >> 63
        };
        self.board[base + word] << 1 | carry
    }

    /// Row `row`'s word `word`, each bit replaced by its eastern neighbour,
    /// the board wrapping round.
    fn east(&self, row: usize, word: usize) -> u64 {
        let base = row * self.words;
        let shifted = self.board[base + word] >> 1;
        if word + 1 == self.words {
            let top = (self.cols - 1) % 64;
            shifted | (self.board[base] & 1) << top
        } else {
            shifted | (self.board[base + word + 1] & 1) << 63
        }
    }

    /// Note how much a generation changed, and reseed a world that has
    /// settled into a cycle, near-stillness, or near-emptiness.
    fn watch_for_settling(&mut self, changed: usize) {
        let cells = self.cols * self.rows;
        let population: usize = self.board.iter().map(|w| w.count_ones() as usize).sum();
        let digest = digest(&self.board);
        let cycling = self.history[..self.recorded.min(HISTORY)].contains(&digest);
        self.history[self.recorded % HISTORY] = digest;
        self.recorded += 1;
        if changed < (cells / 1_500).max(2) {
            self.quiet += 1;
        } else {
            self.quiet = 0;
        }
        let settled = cycling || self.quiet >= QUIET_LIMIT || population < cells / 400;
        self.settled = if settled { self.settled + 1 } else { 0 };
        if self.settled >= SETTLED_GRACE {
            self.reseed();
        }
    }

    /// Scatter a fresh random soup over the board: what was alive fades out,
    /// and the soup is born over it.
    fn reseed(&mut self) {
        for row in 0..self.rows {
            for col in 0..self.cols {
                let cell = row * self.cols + col;
                let alive = self.rng.next_below(1_000) < SEED_DENSITY;
                let bit = 1u64 << (col % 64);
                let word = &mut self.board[row * self.words + col / 64];
                let was = *word & bit != 0;
                if alive {
                    *word |= bit;
                    self.age[cell] = 0;
                    self.family[cell] =
                        u8::try_from(self.rng.next_below(FAMILIES as u64)).unwrap_or(0);
                } else {
                    *word &= !bit;
                }
                if alive || was {
                    self.queue(cell);
                }
            }
        }
        self.recorded = 0;
        self.quiet = 0;
        self.settled = 0;
    }

    /// Put `cell` on the next frame's list, once.
    fn queue(&mut self, cell: usize) {
        let (word, bit) = (cell / 64, 1u64 << (cell % 64));
        if self.queued[word] & bit == 0 {
            self.queued[word] |= bit;
            if let Ok(cell) = u32::try_from(cell) {
                self.active.push(cell);
            }
        }
    }

    /// Step every changing cell's brightness towards its state and mark
    /// where it is drawn.
    fn fade(&mut self) {
        self.damage.clear();
        for index in 0..self.active.len() {
            let cell = self.active[index] as usize;
            let (row, col) = (cell / self.cols, cell % self.cols);
            let level = self.level[cell];
            self.level[cell] = if self.alive(row, col) {
                level.saturating_add(self.steps.0)
            } else {
                level.saturating_sub(self.steps.1)
            };
            self.damage.add(self.cell_rect(row, col));
        }
    }

    /// Drop from the list every cell whose look has settled: fully born and
    /// in its current shade, or fully faded.
    fn retire_settled(&mut self) {
        let mut kept = 0;
        for index in 0..self.active.len() {
            let cell = self.active[index] as usize;
            let (row, col) = (cell / self.cols, cell % self.cols);
            let target = if self.alive(row, col) { u8::MAX } else { 0 };
            if self.level[cell] == target {
                self.queued[cell / 64] &= !(1u64 << (cell % 64));
            } else {
                self.active[kept] = self.active[index];
                kept += 1;
            }
        }
        self.active.truncate(kept);
    }

    /// Repaint every cell `rect` covers: the gap black, a cell in its colour
    /// at its brightness.
    fn paint(&self, surface: &mut Surface, rect: Rect) {
        let (Ok(x), Ok(y)) = (u32::try_from(rect.left()), u32::try_from(rect.top())) else {
            return;
        };
        surface.fill_rect(x, y, rect.width, rect.height, Color::rgb(0, 0, 0));
        let cells = |from: u32, extent: u32, origin: u32, count: usize| {
            let first = from.saturating_sub(origin) / self.cell;
            let last = (from + extent).saturating_sub(origin).div_ceil(self.cell);
            (first as usize).min(count)..(last as usize).min(count)
        };
        let rows = cells(y, rect.height, self.origin.1, self.rows);
        let cols = cells(x, rect.width, self.origin.0, self.cols);
        surface.with_clip(x, y, rect.width, rect.height, |surface| {
            for row in rows {
                for col in cols.clone() {
                    self.paint_cell(surface, row, col);
                }
            }
        });
    }

    /// Lay the ground over one cell and draw it as it looks now.
    fn repaint_cell(&self, surface: &mut Surface, row: usize, col: usize) {
        let Rect { origin, .. } = self.cell_rect(row, col);
        let (Ok(x), Ok(y)) = (u32::try_from(origin.x), u32::try_from(origin.y)) else {
            return;
        };
        surface.fill_rect(x, y, self.cell, self.cell, Color::rgb(0, 0, 0));
        self.paint_cell(surface, row, col);
    }

    /// Draw one cell as the board and its fade say it looks now.
    fn paint_cell(&self, surface: &mut Surface, row: usize, col: usize) {
        let cell = row * self.cols + col;
        let level = self.level[cell];
        if level == 0 {
            return;
        }
        let family = usize::from(self.family[cell]) % FAMILIES;
        let shade = shade_of(self.age[cell]);
        let base = self.palette[family][shade];
        let ink = Color::rgba(base.r, base.g, base.b, level);
        let Rect { origin, .. } = self.cell_rect(row, col);
        let (Ok(x), Ok(y)) = (u32::try_from(origin.x), u32::try_from(origin.y)) else {
            return;
        };
        let side = self.cell - self.gap;
        surface.fill_round_rect(
            x + self.gap / 2,
            y + self.gap / 2,
            side,
            side,
            self.radius,
            ink,
        );
    }

    /// Where the cell at `(row, col)` is on the screen.
    fn cell_rect(&self, row: usize, col: usize) -> Rect {
        let at = |index: usize, origin: u32| {
            i32::try_from(u64::from(origin) + index as u64 * u64::from(self.cell)).unwrap_or(0)
        };
        Rect::new(
            at(col, self.origin.0),
            at(row, self.origin.1),
            self.cell,
            self.cell,
        )
    }
}

/// The eight neighbour words summed bit by bit, as the low three bits of each
/// cell's count: `(ones, twos, fours)`. A count of eight wraps to nought,
/// which neither births nor keeps a cell, exactly as eight does.
fn neighbour_count(n: [u64; 8]) -> (u64, u64, u64) {
    let (a0, a1) = full_add(n[0], n[1], n[2]);
    let (b0, b1) = full_add(n[3], n[4], n[5]);
    let (c0, c1) = (n[6] ^ n[7], n[6] & n[7]);
    let (ones, carry) = full_add(a0, b0, c0);
    let (twos_a, fours_a) = full_add(a1, b1, c1);
    let (twos, fours_b) = (twos_a ^ carry, twos_a & carry);
    (ones, twos, fours_a ^ fours_b)
}

/// One bit-sliced full adder: the sum and carry of three words.
const fn full_add(a: u64, b: u64, c: u64) -> (u64, u64) {
    let partial = a ^ b;
    (partial ^ c, (a & b) | (c & partial))
}

/// A digest of the board, to recognise a generation seen before.
fn digest(board: &[u64]) -> u64 {
    board.iter().fold(0x9E37_79B9_7F4A_7C15, |hash, word| {
        (hash ^ word)
            .rotate_left(23)
            .wrapping_mul(0x2545_F491_4F6C_DD1D)
    })
}

/// Which shade a cell of `age` generations is drawn in.
fn shade_of(age: u8) -> usize {
    SHADE_AGES
        .iter()
        .rposition(|start| age >= *start)
        .unwrap_or(0)
}

/// Every colony's colour in every shade.
fn palette() -> [[Color; SHADE_AGES.len()]; FAMILIES] {
    HUES.map(|(r, g, b)| {
        SHADE_MIX.map(|mix| {
            let toward = |channel: u8| {
                let channel = i32::from(channel);
                let mix = i32::from(mix);
                let moved = if mix >= 0 {
                    channel + (255 - channel) * mix / 255
                } else {
                    channel + channel * mix / 255
                };
                u8::try_from(moved.clamp(0, 255)).unwrap_or(u8::MAX)
            };
            Color::rgb(toward(r), toward(g), toward(b))
        })
    })
}

#[cfg(test)]
#[path = "life_tests.rs"]
mod tests;
