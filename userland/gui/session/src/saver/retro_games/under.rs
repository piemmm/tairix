//! What lies under the craft over the sky: the pixels beneath each box they
//! reach above the horizon, kept as the frame draws them so the next frame
//! lays the sky and the mountains back there by copying them rather than by
//! painting them again.
//!
//! The ranges near the horizon are hundreds of tiny faces, so painting them
//! again under a box costs every face reaching it; copying costs the box.

use alloc::vec::Vec;

use tairix_raster::Pixel;
use tairix_util::fallible;
use tairix_wm::{Rect, Surface};

/// The pixels beneath the boxes the craft last reached over the sky.
pub(super) struct Under {
    boxes: Vec<Rect>,
    /// Every box's rows in turn, top to bottom.
    pixels: Vec<Pixel>,
    /// Whether the pixels hold all of every box: `false` once the heap would
    /// not give them room, when only painting the sky whole lays it back.
    whole: bool,
}

impl Under {
    /// Nothing kept yet, which is all there is to keep before the first
    /// frame.
    pub(super) const fn new() -> Self {
        Self {
            boxes: Vec::new(),
            pixels: Vec::new(),
            whole: true,
        }
    }

    /// The boxes whose pixels are kept.
    pub(super) fn boxes(&self) -> &[Rect] {
        &self.boxes
    }

    /// Whether every box's pixels are kept, so copying them back lays the
    /// sky back wholly.
    pub(super) const fn is_whole(&self) -> bool {
        self.whole
    }

    /// Keep `surface`'s pixels beneath every one of `boxes`; what was kept
    /// before is let go. Kept before anything is drawn over them, boxes that
    /// overlap keep the same pixels twice, and lay them back alike.
    pub(super) fn keep(&mut self, surface: &Surface, boxes: &[Rect]) {
        self.boxes.clear();
        self.pixels.clear();
        let area = boxes
            .iter()
            .map(|rect| {
                usize::try_from(u64::from(rect.width) * u64::from(rect.height))
                    .unwrap_or(usize::MAX)
            })
            .fold(0usize, usize::saturating_add);
        self.whole = fallible::reserve(&mut self.boxes, boxes.len())
            && fallible::reserve(&mut self.pixels, area);
        if !self.whole {
            return;
        }
        for rect in boxes {
            let Some((x, y)) = rect.surface_origin() else {
                continue;
            };
            for row in y..y.saturating_add(rect.height) {
                if let Some((_, span)) = surface.row_span(row, x, rect.width) {
                    self.pixels.extend_from_slice(span);
                }
            }
            self.boxes.push(*rect);
        }
    }

    /// Copy every kept box's pixels back onto `surface`, where they were
    /// kept from.
    pub(super) fn lay_back(&self, surface: &mut Surface) {
        let mut kept = self.pixels.as_slice();
        for rect in &self.boxes {
            let Some((x, y)) = rect.surface_origin() else {
                continue;
            };
            for row in y..y.saturating_add(rect.height) {
                let Some((_, span)) = surface.row_span_mut(row, x, rect.width) else {
                    continue;
                };
                let Some((these, rest)) = kept.split_at_checked(span.len()) else {
                    return;
                };
                span.copy_from_slice(these);
                kept = rest;
            }
        }
    }
}

#[cfg(test)]
#[path = "under_tests.rs"]
mod tests;
