//! Text being set into a picture: what is typed, where the caret is, and
//! the coverage it will lay, drawn through the font service's glyphs.
//!
//! The coverage is made again only when the text changes, at the cost of the
//! text's own size; a paint and the commit read it as it stands.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_font::BitmapFont;
use tairix_raster::{Color, Surface};
use tairix_util::fallible;

use crate::canvas::OutOfMemory;
use crate::shape::Bounds;

/// The most characters one piece of text holds, which bounds what a key
/// press costs to set again.
pub const MOST_CHARS: usize = 512;

/// Text being typed into the picture.
#[derive(Debug)]
pub struct TextEntry {
    /// The top left of its first line, in picture pixels.
    at: (i64, i64),
    text: String,
    /// The caret, a byte offset into `text` on a character's boundary.
    caret: usize,
    /// What the text lays: one coverage a pixel over `bounds`, row by row.
    bounds: Bounds,
    coverage: Vec<u8>,
    /// Where the caret is drawn, in picture pixels: a column and the rows
    /// of its line.
    caret_at: (i64, i64, i64),
}

impl TextEntry {
    /// Empty text with its first line's top left at picture pixel `at`.
    #[must_use]
    pub const fn new(at: (i64, i64)) -> Self {
        Self {
            at,
            text: String::new(),
            caret: 0,
            bounds: Bounds {
                x0: at.0,
                y0: at.1,
                x1: at.0,
                y1: at.1,
            },
            coverage: Vec::new(),
            caret_at: (at.0, at.1, at.1),
        }
    }

    /// What has been typed.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Whether nothing has been typed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// The pixels the text covers.
    #[must_use]
    pub const fn bounds(&self) -> Bounds {
        self.bounds
    }

    /// Where the caret is drawn: its column and the rows of its line, in
    /// picture pixels.
    #[must_use]
    pub const fn caret(&self) -> (i64, i64, i64) {
        self.caret_at
    }

    /// How much of pixel `(x, y)` the text covers.
    #[must_use]
    pub fn at(&self, x: i64, y: i64) -> u8 {
        let b = self.bounds;
        if x < b.x0 || x >= b.x1 || y < b.y0 || y >= b.y1 {
            return 0;
        }
        usize::try_from((y - b.y0) * (b.x1 - b.x0) + (x - b.x0))
            .ok()
            .and_then(|at| self.coverage.get(at))
            .copied()
            .unwrap_or(0)
    }

    /// Write how much of row `y`, from column `x`, the text covers into
    /// `out`.
    pub fn row(&self, y: i64, x: i64, out: &mut [u8]) {
        out.fill(0);
        let b = self.bounds;
        if y < b.y0 || y >= b.y1 {
            return;
        }
        let end = x.saturating_add(i64::try_from(out.len()).unwrap_or(i64::MAX));
        let (from, to) = (b.x0.max(x), b.x1.min(end));
        let (Ok(start), Ok(len), Ok(width)) = (
            usize::try_from(from - x),
            usize::try_from(to - from),
            usize::try_from(b.x1 - b.x0),
        ) else {
            return;
        };
        let row = usize::try_from(y - b.y0).unwrap_or(0) * width;
        let column = usize::try_from(from - b.x0).unwrap_or(0);
        if let (Some(into), Some(held)) = (
            out.get_mut(start..start + len),
            self.coverage.get(row + column..row + column + len),
        ) {
            into.copy_from_slice(held);
        }
    }

    /// Type `ch` at the caret, answering whether there was room for it.
    pub fn insert(&mut self, ch: char) -> bool {
        if self.text.chars().count() >= MOST_CHARS || self.text.try_reserve(ch.len_utf8()).is_err()
        {
            return false;
        }
        self.text.insert(self.caret, ch);
        self.caret += ch.len_utf8();
        true
    }

    /// Take back the character before the caret, answering whether there was
    /// one.
    pub fn backspace(&mut self) -> bool {
        let Some((at, _)) = self.text[..self.caret].char_indices().next_back() else {
            return false;
        };
        self.text.remove(at);
        self.caret = at;
        true
    }

    /// Take away the character after the caret, answering whether there was
    /// one.
    pub fn delete(&mut self) -> bool {
        if self.caret >= self.text.len() {
            return false;
        }
        self.text.remove(self.caret);
        true
    }

    /// Move the caret a character back, or on when `forward`, answering
    /// whether it moved.
    pub fn step(&mut self, forward: bool) -> bool {
        let next = if forward {
            self.text[self.caret..]
                .chars()
                .next()
                .map(|ch| self.caret + ch.len_utf8())
        } else {
            self.text[..self.caret]
                .char_indices()
                .next_back()
                .map(|(at, _)| at)
        };
        match next {
            Some(next) => {
                self.caret = next;
                true
            }
            None => false,
        }
    }

    /// Move the caret to the start of its line, or the end when `end`.
    pub fn to_line_edge(&mut self, end: bool) {
        let start = self.text[..self.caret].rfind('\n').map_or(0, |at| at + 1);
        self.caret = if end {
            self.text[self.caret..]
                .find('\n')
                .map_or(self.text.len(), |at| self.caret + at)
        } else {
            start
        };
    }

    /// Set the text again in `face`, its edges smoothed when `smooth`,
    /// rebuilding the coverage it lays and where its caret is drawn.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`] when the coverage cannot be held; what it laid
    /// before stays.
    pub fn set(&mut self, face: BitmapFont, smooth: bool) -> Result<(), OutOfMemory> {
        let line = face.line_height().max(1);
        let lines = self.text.split('\n');
        let width = lines
            .clone()
            .map(|text| face.text_width(text))
            .max()
            .unwrap_or(0);
        let count = u32::try_from(lines.clone().count()).unwrap_or(u32::MAX);
        let height = line.saturating_mul(count);
        let caret_line = self.text[..self.caret].matches('\n').count();
        let line_start = self.text[..self.caret].rfind('\n').map_or(0, |at| at + 1);
        let caret_x = face.width_to_offset(&self.text[line_start..], self.caret - line_start);
        let top = self.at.1 + i64::from(line) * i64::try_from(caret_line).unwrap_or(0);
        self.caret_at = (self.at.0 + i64::from(caret_x), top, top + i64::from(line));
        if width == 0 {
            self.bounds = Bounds {
                x0: self.at.0,
                y0: self.at.1,
                x1: self.at.0,
                y1: self.at.1,
            };
            self.coverage.clear();
            return Ok(());
        }
        let mut surface = Surface::new(width, height).ok_or(OutOfMemory)?;
        for (index, text) in lines.enumerate() {
            let y = i32::try_from(line)
                .unwrap_or(i32::MAX)
                .saturating_mul(i32::try_from(index).unwrap_or(i32::MAX));
            face.draw_text(&mut surface, 0, y, text, Color::rgba(255, 255, 255, 255));
        }
        let area =
            usize::try_from(u64::from(width) * u64::from(height)).map_err(|_| OutOfMemory)?;
        let mut coverage = fallible::filled(area, 0u8).ok_or(OutOfMemory)?;
        for (y, row) in (0..height).zip(coverage.chunks_exact_mut(width as usize)) {
            for (x, cover) in (0..width).zip(row.iter_mut()) {
                let alpha = surface.get(x, y).map_or(0, |pixel| pixel.a);
                *cover = match (smooth, alpha) {
                    (true, alpha) => alpha,
                    (false, 128..) => u8::MAX,
                    (false, _) => 0,
                };
            }
        }
        self.bounds = Bounds {
            x0: self.at.0,
            y0: self.at.1,
            x1: self.at.0 + i64::from(width),
            y1: self.at.1 + i64::from(height),
        };
        self.coverage = coverage;
        Ok(())
    }
}

#[cfg(test)]
#[path = "text_tests.rs"]
mod tests;
