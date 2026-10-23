//! Bands of whole columns of a surface, each borrowed exclusively: how a
//! paint whose work runs down its columns is spread over several cores, the
//! transpose of [`RowBand`](super::RowBand).
//!
//! A band's pixels are not contiguous, so it holds the rows it spans as a raw
//! pointer and hands out one row's span at a time. That view is this module's
//! alone.

use core::marker::PhantomData;
use core::ops::Range;
use core::ptr::NonNull;

use super::{span_offsets, ClipRect, Origin, Surface};
use crate::color::Pixel;

impl Surface {
    /// Split drawing rows `rows` into bands of `columns_per_band` whole
    /// columns each, from drawing column `columns.start` to `columns.end`,
    /// every band borrowed exclusively: [`row_bands_mut`](Self::row_bands_mut)
    /// turned on its side, for a paint that sums down each column.
    ///
    /// A band honours the clip window and the stated origin exactly as the
    /// surface does. Rows and columns outside the surface or the window are
    /// dropped, so a caller need not pre-clamp; `columns_per_band` of `0`
    /// reads as `1`, and nothing admitted yields no bands.
    pub fn column_bands_mut(
        &mut self,
        rows: Range<u32>,
        columns: Range<u32>,
        columns_per_band: u32,
    ) -> ColumnBands<'_> {
        let (row, count) = self
            .origin
            .rows(rows.start, rows.end.saturating_sub(rows.start));
        let rows = self.clip.rows(row, count);
        let (column, count) = self
            .origin
            .columns(columns.start, columns.end.saturating_sub(columns.start));
        let columns = self
            .clip
            .columns(column, count)
            .filter(|_| !rows.is_empty())
            .unwrap_or(0..0);
        let width = self.width;
        let start = usize::try_from(u64::from(rows.start) * u64::from(width)).unwrap_or(0);
        let len = usize::try_from(u64::from(rows.end - rows.start) * u64::from(width)).unwrap_or(0);
        let block = self
            .pixels_mut()
            .get_mut(start..start.saturating_add(len))
            .unwrap_or_default();
        ColumnBands {
            pixels: NonNull::from(&mut *block).cast::<Pixel>(),
            len: block.len(),
            width,
            rows,
            next: columns.start,
            end: columns.end,
            per_band: columns_per_band.max(1),
            clip: self.clip,
            origin: self.origin,
            borrow: PhantomData,
        }
    }
}

/// The bands [`Surface::column_bands_mut`] splits a block of rows into.
pub struct ColumnBands<'a> {
    /// The block's first pixel: the first column of its first row.
    pixels: NonNull<Pixel>,
    len: usize,
    width: u32,
    /// The buffer rows the block holds.
    rows: Range<u32>,
    /// The buffer column the next band starts at, and the one the last ends
    /// at.
    next: u32,
    end: u32,
    per_band: u32,
    clip: ClipRect,
    origin: Origin,
    borrow: PhantomData<&'a mut [Pixel]>,
}

impl<'a> Iterator for ColumnBands<'a> {
    type Item = ColumnBand<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next >= self.end {
            return None;
        }
        let start = self.next;
        let end = start.saturating_add(self.per_band).min(self.end);
        self.next = end;
        Some(ColumnBand {
            pixels: self.pixels,
            len: self.len,
            width: self.width,
            rows: self.rows.clone(),
            clip: self.clip.narrowed(start, 0, end - start, u32::MAX),
            origin: self.origin,
            borrow: PhantomData,
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let bands = usize::try_from(self.end.saturating_sub(self.next).div_ceil(self.per_band))
            .unwrap_or(usize::MAX);
        (bands, Some(bands))
    }
}

impl ExactSizeIterator for ColumnBands<'_> {}

/// One band of [`Surface::column_bands_mut`]: a run of whole columns of a
/// block of surface rows, borrowed exclusively.
///
/// A band is written exactly as the surface is — [`row_span_mut`] honours the
/// same clip window and yields the same pixels — but only for the columns it
/// owns, which is what lets a pass write several bands at once.
///
/// [`row_span_mut`]: ColumnBand::row_span_mut
pub struct ColumnBand<'a> {
    pixels: NonNull<Pixel>,
    len: usize,
    width: u32,
    rows: Range<u32>,
    /// The surface's window narrowed to the band's columns.
    clip: ClipRect,
    origin: Origin,
    borrow: PhantomData<&'a mut [Pixel]>,
}

// SAFETY: a band is an exclusive borrow of its columns of its rows, as a
// `&mut [Pixel]` is of its pixels, and `Pixel` is `Send`; it holds a pointer
// only because those pixels are not contiguous.
#[allow(unsafe_code)] // Sending the exclusive borrow, proven above.
unsafe impl Send for ColumnBand<'_> {}

impl ColumnBand<'_> {
    /// The surface rows this band spans, in the paint's own coordinates.
    #[must_use]
    pub fn rows(&self) -> Range<u32> {
        self.origin.space_row(self.rows.start)..self.origin.space_row(self.rows.end)
    }

    /// The columns this band owns, in the paint's own coordinates.
    #[must_use]
    pub fn columns(&self) -> Range<u32> {
        self.origin.space_column(self.clip.x0)..self.origin.space_column(self.clip.x1)
    }

    /// Borrow the writable pixels of row `y` from column `x`, for at most `w`
    /// columns, with the column the returned span actually starts at.
    ///
    /// `None` for a row this band does not span or columns it does not own,
    /// and otherwise exactly what [`Surface::row_span_mut`] answers for the
    /// same arguments, cut to the band's columns.
    #[must_use]
    pub fn row_span_mut(&mut self, y: u32, x: u32, w: u32) -> Option<(u32, &mut [Pixel])> {
        let place = span_offsets(
            self.clip,
            self.width,
            self.rows.clone(),
            self.origin,
            y,
            x,
            w,
        )?;
        let Range { start, end } = place.offsets;
        if start > end || end > self.len {
            return None;
        }
        Some((place.first, self.span(start, end - start)))
    }

    /// The `len` pixels from offset `at` of the band's block.
    #[allow(unsafe_code)] // The one view of a band's pixels, proven below.
    fn span(&mut self, at: usize, len: usize) -> &mut [Pixel] {
        // SAFETY: `at + len` is within the block `column_bands_mut` borrowed
        // exclusively from its surface for the band's lifetime, checked by the
        // caller. The span lies in this band's columns, which no other band of
        // the split owns, so it overlaps no span another band hands out; it
        // borrows this band mutably, so the band hands out no second one while
        // it lives.
        unsafe { core::slice::from_raw_parts_mut(self.pixels.as_ptr().add(at), len) }
    }
}

#[cfg(test)]
#[path = "columns_tests.rs"]
mod tests;
