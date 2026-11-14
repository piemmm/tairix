//! The canvas every model's picture is painted on: a byte a 16 MHz slot, two
//! rows a line, the frame being drawn and the last one finished, and which of
//! the finished frame's rows changed since they were last taken.

use alloc::vec;
use alloc::vec::Vec;

use crate::snapshot::{Load, Reader, Save, SnapshotError, Writer};

/// Canvas width in slots: 56 µs of each line.
pub const CANVAS_WIDTH: usize = 896;

/// Lines of each frame the canvas holds.
pub const CANVAS_LINES: usize = 300;

/// Canvas height in rows: two per line, an interlaced picture's fields'
/// lines interleaved.
pub const CANVAS_HEIGHT: usize = 2 * CANVAS_LINES;

const SLOTS: usize = CANVAS_WIDTH * CANVAS_HEIGHT;

/// A slot as the canvas stores it: the foreground's colour, the background's,
/// and how much of the slot the foreground covers, 0 to 3. Colours are a bit
/// each of red, green and blue, from bit 0.
#[must_use]
pub const fn encode(foreground: u8, background: u8, coverage: u8) -> u8 {
    (foreground & 7) | ((background & 7) << 3) | ((coverage & 3) << 6)
}

/// A completely black slot.
pub const BLACK: u8 = encode(0, 0, 3);

/// Which canvas rows have changed since they were last taken.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirtyRows {
    words: [u64; CANVAS_HEIGHT.div_ceil(64)],
}

impl Default for DirtyRows {
    fn default() -> Self {
        Self::new()
    }
}

impl DirtyRows {
    /// No row changed.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            words: [0; CANVAS_HEIGHT.div_ceil(64)],
        }
    }

    fn mark(&mut self, row: usize) {
        self.words[row / 64] |= 1 << (row % 64);
    }

    /// Whether `row` changed.
    #[must_use]
    pub const fn contains(&self, row: usize) -> bool {
        row < CANVAS_HEIGHT && self.words[row / 64] & (1 << (row % 64)) != 0
    }

    /// The changed rows, in order.
    pub fn rows(&self) -> impl Iterator<Item = usize> + '_ {
        self.words
            .iter()
            .zip((0..).step_by(64))
            .flat_map(|(&word, first)| {
                let mut bits = word;
                core::iter::from_fn(move || {
                    let bit = bits.trailing_zeros();
                    (bits != 0).then(|| {
                        bits &= bits - 1;
                        first + bit as usize
                    })
                })
            })
    }

    fn all() -> Self {
        let mut rows = Self::new();
        for row in 0..CANVAS_HEIGHT {
            rows.mark(row);
        }
        rows
    }
}

/// The frame being drawn and the last one finished.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Canvas {
    drawing: Vec<u8>,
    shown: Vec<u8>,
    /// The rows painted afresh since the last frame was finished: the only
    /// ones that can differ from it.
    touched: DirtyRows,
    dirty: DirtyRows,
    frames: u64,
}

impl Default for Canvas {
    fn default() -> Self {
        Self::new()
    }
}

impl Canvas {
    /// A black canvas.
    #[must_use]
    pub fn new() -> Self {
        Self {
            drawing: vec![BLACK; SLOTS],
            shown: vec![BLACK; SLOTS],
            touched: DirtyRows::new(),
            dirty: DirtyRows::new(),
            frames: 0,
        }
    }

    /// Paint `slots` into row `row` of the frame being drawn from slot
    /// `first`; what lies past the canvas is dropped.
    pub fn paint(&mut self, row: usize, first: usize, slots: &[u8]) {
        if row >= CANVAS_HEIGHT || first >= CANVAS_WIDTH {
            return;
        }
        let len = slots.len().min(CANVAS_WIDTH - first);
        let start = row * CANVAS_WIDTH + first;
        let painted = &mut self.drawing[start..start + len];
        if *painted != slots[..len] {
            painted.copy_from_slice(&slots[..len]);
            self.touched.mark(row);
        }
    }

    /// The frame being drawn is finished: each row drawn differently from the
    /// one shown is shown, and is dirty until taken.
    pub fn finish(&mut self) {
        let drawn = self.drawing.as_chunks::<CANVAS_WIDTH>().0;
        let shown = self.shown.as_chunks_mut::<CANVAS_WIDTH>().0;
        for row in self.touched.rows() {
            if drawn[row] != shown[row] {
                shown[row] = drawn[row];
                self.dirty.mark(row);
            }
        }
        self.touched = DirtyRows::new();
        self.frames += 1;
    }

    /// Frames finished since power-on.
    #[must_use]
    pub const fn frames(&self) -> u64 {
        self.frames
    }

    /// The last finished frame, `CANVAS_WIDTH` slots to a row.
    #[must_use]
    pub fn shown(&self) -> &[u8] {
        &self.shown
    }

    /// The rows of the finished frame that changed since this was last
    /// called.
    pub fn take_dirty(&mut self) -> DirtyRows {
        core::mem::take(&mut self.dirty)
    }
}

impl Save for Canvas {
    fn save(&self, w: &mut Writer) {
        w.bytes(&self.drawing);
        w.bytes(&self.shown);
        w.put(&self.frames);
    }
}

impl Load for Canvas {
    /// Every row of a restored canvas is new to whoever shows it, and no
    /// machine finishes more frames than it has run cycles.
    fn load(r: &mut Reader<'_>) -> Result<Self, SnapshotError> {
        fn frame(r: &mut Reader<'_>) -> Result<Vec<u8>, SnapshotError> {
            let frame = r.bytes_at_most(SLOTS)?;
            Reader::check(frame.len() == SLOTS)?;
            Ok(frame.to_vec())
        }
        let canvas = Self {
            drawing: frame(r)?,
            shown: frame(r)?,
            touched: DirtyRows::all(),
            dirty: DirtyRows::all(),
            frames: r.get()?,
        };
        Reader::check(canvas.frames <= r.clock())?;
        Ok(canvas)
    }
}

#[cfg(test)]
#[path = "canvas_tests.rs"]
mod tests;
